-- Demo schema for the live window. Deterministic: no random(), so the saved
-- results in fallback.json always match what PGlite returns.
create schema shop;

create type shop.order_status as enum ('pending', 'paid', 'shipped', 'delivered', 'cancelled', 'refunded');

create table shop.customers (
  id        integer primary key,
  full_name text not null,
  email     text not null unique,
  country   char(2) not null,
  signed_up date not null
);

insert into shop.customers
select i,
       (array['Grace','Ada','Frances','Edsger','Radia','Ken','Barbara','Dennis','Margaret','Linus'])[1 + i % 10]
         || ' ' ||
       (array['Hopper','Lovelace','Allen','Dijkstra','Perlman','Thompson','Liskov','Ritchie','Hamilton','Torvalds','Knuth'])[1 + (i * 7) % 11],
       'customer' || i || '@example.com',
       (array['FR','DE','US','GB','ES','NL','SE'])[1 + (i * 3) % 7],
       date '2023-01-01' + i * 5
from generate_series(1, 120) as i;

create table shop.products (
  id       integer primary key,
  name     text not null,
  category text not null,
  price    numeric(10,2) not null check (price > 0),
  stock    integer not null,
  tags     text[] not null,
  specs    jsonb
);

insert into shop.products
select i + 1,
       (array['Walnut','Steel','Linen','Cork','Brass','Glass'])[1 + i % 6]
         || ' ' ||
       (array['desk lamp','notebook','keyboard tray','monitor stand'])[1 + (i / 6) % 4],
       (array['lighting','paper','desk','desk'])[1 + (i / 6) % 4],
       (9 + (i * 37) % 180)::numeric + 0.90,
       (i * 13) % 90,
       array[(array['new','classic','limited'])[1 + i % 3], (array['eu','us'])[1 + i % 2]],
       case when i % 5 = 4 then null
            else jsonb_build_object('weight_g', 120 + i * 35, 'colour', (array['oak','black','sand'])[1 + i % 3])
       end
from generate_series(0, 23) as i;

create table shop.orders (
  id          integer primary key,
  customer_id integer not null references shop.customers (id),
  status      shop.order_status not null,
  total       numeric(10,2) not null,
  currency    char(3) not null,
  placed_at   timestamptz not null,
  shipped_at  timestamptz
);

insert into shop.orders
select i,
       1 + (i * 31) % 120,
       (enum_range(null::shop.order_status))[1 + (i * 5) % 6],
       round(((i * 7919) % 60000) / 100.0 + 5, 2),
       (array['EUR','USD','GBP'])[1 + (i * 2) % 3],
       timestamptz '2024-01-01 00:00+00' + i * interval '53 minutes',
       case when (i * 5) % 6 in (2, 3)
            then timestamptz '2024-01-01 00:00+00' + i * interval '53 minutes' + interval '1 day'
       end
from generate_series(1, 2000) as i;

create table shop.order_items (
  order_id   integer not null references shop.orders (id) on delete cascade,
  product_id integer not null references shop.products (id),
  quantity   integer not null check (quantity > 0),
  unit_price numeric(10,2) not null,
  primary key (order_id, product_id)
);

insert into shop.order_items
select o, p.id, 1 + (o + k) % 4, p.price
from generate_series(1, 2000) as o
cross join generate_series(0, 2) as k
join shop.products p on p.id = 1 + (o * 7 + k * 5) % 24
where k < 1 + o % 3;

create view shop.revenue_by_country as
select c.country, count(*) as orders, sum(o.total) as revenue
from shop.orders o
join shop.customers c on c.id = o.customer_id
where o.status in ('paid', 'shipped', 'delivered')
group by c.country;

analyze;

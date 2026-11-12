-- Kerno test fixture: PostgreSQL sample data. Deterministic.

INSERT INTO shop.categories (id, parent_id, name, slug) VALUES
    (1, NULL, 'Coffee', 'coffee'),
    (2, NULL, 'Tea', 'tea'),
    (3, NULL, 'Equipment', 'equipment'),
    (4, 1, 'Single origin', 'coffee-single-origin'),
    (5, 1, 'Blends', 'coffee-blends'),
    (6, 2, 'Green tea', 'tea-green'),
    (7, 2, 'Black tea', 'tea-black'),
    (8, 3, 'Grinders', 'equipment-grinders'),
    (9, 3, 'Brewers', 'equipment-brewers'),
    (10, 3, 'Accessories', 'equipment-accessories');
SELECT setval(pg_get_serial_sequence('shop.categories', 'id'), 10);

INSERT INTO shop.customers (email, full_name, phone, is_active, tags, preferences, shipping_address, created_at, updated_at)
SELECT 'customer' || g || '@example.com',
       (ARRAY['Ada', 'Grace', 'Linus', 'Margaret', 'Dennis', 'Barbara', 'Ken', 'Radia', 'Edsger', 'Frances'])[1 + g % 10]
           || ' ' ||
       (ARRAY['Lovelace', 'Hopper', 'Torvalds', 'Hamilton', 'Ritchie', 'Liskov', 'Thompson', 'Perlman', 'Dijkstra', 'Allen'])[1 + (g / 10) % 10],
       CASE WHEN g % 7 = 0 THEN NULL ELSE '+33 6 ' || lpad((g * 7919 % 100000000)::text, 8, '0') END,
       g % 13 <> 0,
       CASE g % 4 WHEN 0 THEN ARRAY['vip', 'newsletter'] WHEN 1 THEN ARRAY['newsletter'] WHEN 2 THEN ARRAY[]::text[] ELSE ARRAY['wholesale'] END,
       jsonb_build_object('language', (ARRAY['en', 'fr', 'de', 'es'])[1 + g % 4], 'marketing', g % 3 = 0, 'grind', (ARRAY['whole', 'filter', 'espresso'])[1 + g % 3]),
       CASE WHEN g % 5 = 0 THEN NULL
            ELSE ROW(g || ' rue de la Paix', (ARRAY['Paris', 'Lyon', 'Lille', 'Nantes', 'Berlin', 'Madrid'])[1 + g % 6], lpad((g * 37 % 100000)::text, 5, '0'), (ARRAY['FR', 'FR', 'FR', 'FR', 'DE', 'ES'])[1 + g % 6])::shop.address END,
       TIMESTAMPTZ '2023-01-01 08:00:00+00' + (g || ' hours')::interval,
       TIMESTAMPTZ '2023-01-01 08:00:00+00' + (g || ' hours')::interval
FROM generate_series(1, 500) g;

INSERT INTO shop.products (public_id, category_id, sku, name, description, price, stock, weight_kg, attributes, tags, dimensions, released_on, discontinued)
SELECT md5('product-' || g)::uuid,
       4 + g % 7,
       'SKU-' || lpad(g::text, 5, '0'),
       (ARRAY['Ethiopia Guji', 'Colombia Huila', 'Kenya AA', 'House Blend', 'Sencha', 'Assam', 'Burr Grinder', 'Pour-over Kit', 'Paper Filters', 'Gooseneck Kettle'])[1 + g % 10] || ' #' || g,
       CASE WHEN g % 9 = 0 THEN NULL ELSE 'Batch ' || g || E'.\nRoasted to order, shipped within two days.' END,
       (4.5 + (g * 37 % 9000) / 100.0)::numeric(10, 2),
       g * 11 % 250,
       CASE WHEN g % 6 = 0 THEN NULL ELSE (0.25 + (g % 20) * 0.05)::real END,
       jsonb_build_object('origin', (ARRAY['ET', 'CO', 'KE', 'BR', 'JP', 'IN'])[1 + g % 6], 'organic', g % 2 = 0, 'notes', jsonb_build_array('citrus', 'cocoa', 'floral')),
       CASE g % 3 WHEN 0 THEN ARRAY['new'] WHEN 1 THEN ARRAY['bestseller', 'gift'] ELSE ARRAY[]::text[] END,
       CASE WHEN g % 4 = 0 THEN NULL ELSE ARRAY[10 + g % 5, 20.5, 7.25]::numeric(8, 2)[] END,
       DATE '2021-01-01' + (g * 3),
       g % 17 = 0
FROM generate_series(1, 200) g;

INSERT INTO shop.orders (customer_id, status, placed_at, shipped_at, total, currency, notes)
SELECT 1 + (g * 31) % 500,
       (ARRAY['pending', 'paid', 'shipped', 'delivered', 'delivered', 'cancelled', 'refunded'])[1 + g % 7]::shop.order_status,
       TIMESTAMPTZ '2024-01-01 00:00:00+00' + (g * 53 || ' minutes')::interval,
       CASE WHEN g % 7 IN (2, 3, 4) THEN TIMESTAMP '2024-01-02 00:00:00' + (g * 53 || ' minutes')::interval END,
       0,
       (ARRAY['EUR', 'EUR', 'USD', 'GBP'])[1 + g % 4],
       CASE WHEN g % 11 = 0 THEN 'Leave with the neighbour' END
FROM generate_series(1, 5000) g;

INSERT INTO shop.order_items (order_id, line_no, product_id, quantity, unit_price)
SELECT o.id,
       l,
       1 + (o.id * 7 + l * 13) % 200,
       1 + (o.id + l) % 4,
       (4.5 + ((1 + (o.id * 7 + l * 13) % 200) * 37 % 9000) / 100.0)::numeric(10, 2)
FROM shop.orders o
CROSS JOIN generate_series(1, 4) l
WHERE l <= 1 + o.id % 4;

UPDATE shop.orders o
SET total = t.total
FROM (SELECT order_id, sum(quantity * unit_price) AS total FROM shop.order_items GROUP BY order_id) t
WHERE t.order_id = o.id;

INSERT INTO shop.reviews (product_id, customer_id, rating, title, body, created_at)
SELECT 1 + g % 200,
       1 + (g * 3) % 500,
       1 + g % 5,
       CASE WHEN g % 4 = 0 THEN NULL ELSE 'Review ' || g END,
       repeat('Good value, would order again. ', 1 + g % 6),
       TIMESTAMPTZ '2024-03-01 12:00:00+00' + (g || ' hours')::interval
FROM generate_series(1, 800) g
ON CONFLICT DO NOTHING;

INSERT INTO shop.audit_log (happened_at, actor, action, payload)
SELECT TIMESTAMPTZ '2024-01-01 00:00:00+00' + (g || ' minutes')::interval,
       (ARRAY['system', 'ada', 'grace'])[1 + g % 3],
       (ARRAY['login', 'order_status', 'export', 'login'])[1 + g % 4],
       CASE WHEN g % 5 = 0 THEN NULL ELSE jsonb_build_object('n', g) END
FROM generate_series(1, 300) g;

INSERT INTO analytics.events (customer_id, event_type, properties, occurred_at)
SELECT CASE WHEN g % 9 = 0 THEN NULL ELSE 1 + g % 500 END,
       (ARRAY['page_view', 'add_to_cart', 'checkout', 'search'])[1 + g % 4],
       jsonb_build_object('path', '/products/' || (g % 200), 'ms', g % 900),
       TIMESTAMPTZ '2024-06-01 00:00:00+00' + (g || ' seconds')::interval
FROM generate_series(1, 20000) g;

REFRESH MATERIALIZED VIEW shop.product_sales;

-- all_types: a typical row, a row of NULLs, a row of edge values.
INSERT INTO public.all_types VALUES (
    1, 12345, 2147483647, 9223372036854775807, 12345.678901, 0.1, 3.14, 2.718281828459045, 1234.56, true,
    'abc', 'varchar value', E'Multi-line\ntext with "quotes", a tab\tand unicode: café ☃ \U0001F600',
    '\xdeadbeef00ff', '2024-02-29', '13:37:42.123456', '13:37:42+02', '2024-02-29 13:37:42.123456',
    '2024-02-29 13:37:42.123456+00', '1 year 2 mons 3 days 04:05:06.789', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
    '{"a": 1, "b": [true, null], "c": {"d": "e"}}', '{"a": 1, "b": [true, null], "c": {"d": "e"}}',
    '<root><item id="1">x</item></root>', '192.168.1.10/24', '10.0.0.0/8', '08:00:2b:01:02:03',
    '{1,2,NULL,4}', '{"a","b c","with \"quote\"",NULL}', '{1.5,2.25}', '[1,10)', '[2024-01-01 00:00:00+00,2024-12-31 00:00:00+00)',
    '[2024-01-01,2024-02-01)', '(1.5,-2.5)', B'10101010', B'1011', to_tsvector('english', 'the quick brown fox'),
    'happy', ROW('1 Main St', 'Paris', '75001', 'FR'), 'k1=>v1, k2=>NULL', 16384, 'a_name'
);
INSERT INTO public.all_types (id) VALUES (2);
INSERT INTO public.all_types VALUES (
    3, -32768, -2147483648, -9223372036854775808, -99999999999999.999999, 'NaN', 'Infinity', '-Infinity', -0.01, false,
    '', '', '', '\x', 'infinity', '00:00:00', '23:59:59.999999-12', '-infinity',
    '0001-01-01 00:00:00+00', '-1 days', '00000000-0000-0000-0000-000000000000',
    'null', '[]', '<a/>', '::1', '::/0', 'ff:ff:ff:ff:ff:ff',
    '{}', '{}', '{}', 'empty', '(,)', '[2024-01-01,)', '(0,0)', B'00000000', B'', '',
    'sad', ROW(NULL, NULL, NULL, NULL), '', 0, ''
);

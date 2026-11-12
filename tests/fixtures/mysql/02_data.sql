-- Kerno test fixture: MySQL / MariaDB sample data. Deterministic.
SET NAMES utf8mb4;
USE kerno;

-- Numbers 1..1,000,000, built without engine-specific syntax. Dropped by
-- 03_big.sql once the last generated table is filled.
CREATE TABLE _digits (d INT NOT NULL PRIMARY KEY);
INSERT INTO _digits VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);
CREATE TABLE _seq (g INT NOT NULL PRIMARY KEY);
INSERT INTO _seq
SELECT 1 + a.d + b.d * 10 + c.d * 100 + d.d * 1000 + e.d * 10000 + f.d * 100000
FROM _digits a, _digits b, _digits c, _digits d, _digits e, _digits f;

INSERT INTO categories (id, parent_id, name, slug) VALUES
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

INSERT INTO customers (email, full_name, phone, is_active, tier, channels, preferences, city, country, created_at)
SELECT CONCAT('customer', g, '@example.com'),
       CONCAT(ELT(1 + g % 10, 'Ada', 'Grace', 'Linus', 'Margaret', 'Dennis', 'Barbara', 'Ken', 'Radia', 'Edsger', 'Frances'), ' ',
              ELT(1 + (g DIV 10) % 10, 'Lovelace', 'Hopper', 'Torvalds', 'Hamilton', 'Ritchie', 'Liskov', 'Thompson', 'Perlman', 'Dijkstra', 'Allen')),
       IF(g % 7 = 0, NULL, CONCAT('+33 6 ', LPAD(g * 7919 % 100000000, 8, '0'))),
       g % 13 <> 0,
       ELT(1 + g % 3, 'standard', 'vip', 'wholesale'),
       ELT(1 + g % 4, '', 'email', 'email,sms', 'email,sms,post'),
       JSON_OBJECT('language', ELT(1 + g % 4, 'en', 'fr', 'de', 'es'), 'marketing', g % 3 = 0),
       IF(g % 5 = 0, NULL, ELT(1 + g % 6, 'Paris', 'Lyon', 'Lille', 'Nantes', 'Berlin', 'Madrid')),
       IF(g % 5 = 0, NULL, ELT(1 + g % 6, 'FR', 'FR', 'FR', 'FR', 'DE', 'ES')),
       TIMESTAMP('2023-01-01 08:00:00') + INTERVAL g HOUR
FROM _seq WHERE g <= 500 ORDER BY g;

INSERT INTO products (public_id, category_id, sku, name, description, price, stock, weight_kg, attributes, released_on, discontinued)
SELECT LOWER(CONCAT_WS('-', SUBSTR(MD5(CONCAT('product-', g)), 1, 8), SUBSTR(MD5(CONCAT('product-', g)), 9, 4),
                       SUBSTR(MD5(CONCAT('product-', g)), 13, 4), SUBSTR(MD5(CONCAT('product-', g)), 17, 4),
                       SUBSTR(MD5(CONCAT('product-', g)), 21, 12))),
       4 + g % 7,
       CONCAT('SKU-', LPAD(g, 5, '0')),
       CONCAT(ELT(1 + g % 10, 'Ethiopia Guji', 'Colombia Huila', 'Kenya AA', 'House Blend', 'Sencha', 'Assam', 'Burr Grinder', 'Pour-over Kit', 'Paper Filters', 'Gooseneck Kettle'), ' #', g),
       IF(g % 9 = 0, NULL, CONCAT('Batch ', g, '.\nRoasted to order, shipped within two days.')),
       4.5 + (g * 37 % 9000) / 100,
       g * 11 % 250,
       IF(g % 6 = 0, NULL, 0.25 + (g % 20) * 0.05),
       JSON_OBJECT('origin', ELT(1 + g % 6, 'ET', 'CO', 'KE', 'BR', 'JP', 'IN'), 'organic', g % 2 = 0, 'notes', JSON_ARRAY('citrus', 'cocoa', 'floral')),
       DATE('2021-01-01') + INTERVAL g * 3 DAY,
       g % 17 = 0
FROM _seq WHERE g <= 200 ORDER BY g;

INSERT INTO orders (customer_id, status, placed_at, shipped_at, total, currency, notes)
SELECT 1 + (g * 31) % 500,
       ELT(1 + g % 7, 'pending', 'paid', 'shipped', 'delivered', 'delivered', 'cancelled', 'refunded'),
       TIMESTAMP('2024-01-01 00:00:00') + INTERVAL g * 53 MINUTE,
       IF(g % 7 IN (2, 3, 4), TIMESTAMP('2024-01-02 00:00:00') + INTERVAL g * 53 MINUTE, NULL),
       0,
       ELT(1 + g % 4, 'EUR', 'EUR', 'USD', 'GBP'),
       IF(g % 11 = 0, 'Leave with the neighbour', NULL)
FROM _seq WHERE g <= 5000 ORDER BY g;

INSERT INTO order_items (order_id, line_no, product_id, quantity, unit_price)
SELECT o.id,
       l.g,
       1 + (o.id * 7 + l.g * 13) % 200,
       1 + (o.id + l.g) % 4,
       4.5 + ((1 + (o.id * 7 + l.g * 13) % 200) * 37 % 9000) / 100
FROM orders o
JOIN _seq l ON l.g <= 1 + o.id % 4;

UPDATE orders o
JOIN (SELECT order_id, SUM(quantity * unit_price) AS total FROM order_items GROUP BY order_id) t ON t.order_id = o.id
SET o.total = t.total;

INSERT IGNORE INTO reviews (product_id, customer_id, rating, title, body, created_at)
SELECT 1 + g % 200,
       1 + (g * 3) % 500,
       1 + g % 5,
       IF(g % 4 = 0, NULL, CONCAT('Review ', g)),
       REPEAT('Good value, would order again. ', 1 + g % 6),
       TIMESTAMP('2024-03-01 12:00:00') + INTERVAL g HOUR
FROM _seq WHERE g <= 800 ORDER BY g;

INSERT INTO audit_log (happened_at, actor, action, payload)
SELECT TIMESTAMP('2024-01-01 00:00:00') + INTERVAL g MINUTE,
       ELT(1 + g % 3, 'system', 'ada', 'grace'),
       ELT(1 + g % 4, 'login', 'order_status', 'export', 'login'),
       IF(g % 5 = 0, NULL, JSON_OBJECT('n', g))
FROM _seq WHERE g <= 300 ORDER BY g;

INSERT INTO kerno_analytics.events (customer_id, event_type, properties, occurred_at)
SELECT IF(g % 9 = 0, NULL, 1 + g % 500),
       ELT(1 + g % 4, 'page_view', 'add_to_cart', 'checkout', 'search'),
       JSON_OBJECT('path', CONCAT('/products/', g % 200), 'ms', g % 900),
       TIMESTAMP('2024-06-01 00:00:00') + INTERVAL g SECOND
FROM _seq WHERE g <= 20000 ORDER BY g;

INSERT INTO kerno_other.notes (body) VALUES ('first'), ('second');

-- all_types: a typical row, a row of NULLs, a row of edge values.
INSERT INTO all_types VALUES (
    1, 127, 255, TRUE, 32767, 8388607, 2147483647, 4294967295, 9223372036854775807, 18446744073709551615,
    12345.678901, 3.14, 2.718281828459045, b'10101010', b'1',
    'abc', 'varchar value', 'tiny', 'Multi-line\ntext with "quotes", a tab\tand unicode: café ☃ 😀', 'medium', 'long',
    X'DEADBEEF', X'00FF10', X'DEADBEEF00FF',
    '2024-02-29', '13:37:42.123456', '2024-02-29 13:37:42.123456', '2024-02-29 13:37:42.123456', 2024,
    'happy', 'a,c', '{"a": 1, "b": [true, null], "c": {"d": "e"}}', ST_GeomFromText('POINT(1.5 -2.5)')
);
INSERT INTO all_types (id) VALUES (2);
INSERT INTO all_types VALUES (
    3, -128, 0, FALSE, -32768, -8388608, -2147483648, 0, -9223372036854775808, 0,
    -99999999999999.999999, -1.5e10, 1.7976931348623157e308, b'00000000', b'0',
    '', '', '', '', '', '',
    X'00000000', X'', X'',
    '1000-01-01', '-838:59:59.000000', '9999-12-31 23:59:59.999999', '1970-01-01 00:00:01.000000', 1901,
    'sad', '', '[]', ST_GeomFromText('POINT(0 0)')
);

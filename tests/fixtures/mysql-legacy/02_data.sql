-- Data of the legacy fixture (K22). Deterministic.
SET NAMES utf8;
USE kerno;

INSERT INTO customers (id, email, full_name, is_active, tier, country, created_at, updated_at) VALUES
    (1, 'ana@example.com', 'Ana Lima', 1, 'vip', 'BR', '2024-01-05 10:00:00', '2024-01-05 10:00:00'),
    (2, 'bo@example.com', 'Bo Nilsson', 1, 'standard', 'SE', '2024-02-11 09:30:00', '2024-02-11 09:30:00'),
    (3, 'chloe@example.com', 'Chloé Durand', 0, 'standard', 'FR', '2024-03-20 17:45:00', '2024-03-20 17:45:00');

INSERT INTO categories (id, parent_id, name) VALUES
    (1, NULL, 'Kitchen'),
    (2, 1, 'Knives');

INSERT INTO products (id, category_id, sku, name, price, stock, released_on) VALUES
    (1, 2, 'KN-001', 'Chef knife', 79.90, 12, '2023-05-01'),
    (2, 1, 'KT-002', 'Kettle', 39.00, 0, NULL);

INSERT INTO orders (id, customer_id, status, placed_at, total) VALUES
    (1, 1, 'paid', '2024-04-01 12:00:00', 118.90),
    (2, 2, 'pending', '2024-04-02 08:15:00', 39.00);

INSERT INTO order_items (order_id, line_no, product_id, quantity, unit_price) VALUES
    (1, 1, 1, 1, 79.90),
    (1, 2, 2, 1, 39.00),
    (2, 1, 2, 1, 39.00);

INSERT INTO audit_log (happened_at, actor, action) VALUES
    ('2024-04-01 12:00:01', 'system', 'order 1 created'),
    ('2024-04-01 12:00:01', 'system', 'order 1 created');

INSERT INTO notes (id, body) VALUES
    (1, 'Remember to restock the kettles'),
    (2, 'Café crème for the team');

INSERT INTO kerno_other.things (id, name) VALUES (1, 'one');

INSERT INTO all_types VALUES
    (1, -128, 1, -32768, -8388608, 4294967295, -9223372036854775808, 18446744073709551615,
     -12345678901234.123456, 1.5, 2.25, b'10101010', 'abc', 'Grüße', 'long text',
     X'DEADBEEF', X'00FF', X'0102', '2024-02-29', '-838:59:59', '2024-02-29 23:59:59',
     '2024-02-29 23:59:59', 2024, 'happy', 'a,c'),
    (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

-- Marker read by the container healthcheck: the seed is complete.
CREATE TABLE kerno.kerno_seed_done (done INT NOT NULL PRIMARY KEY);
INSERT INTO kerno.kerno_seed_done VALUES (1);
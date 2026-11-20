-- Kerno test fixture for the legacy MySQL servers (5.1 to 5.7, K22): the
-- shop of tests/fixtures/mysql reduced to what MySQL 5.1.73 accepts. No
-- utf8mb4 (5.5.3), JSON (5.7.8), generated columns (5.7.6), fractional
-- seconds (5.6.4), CHECK, SIGNAL (5.5), InnoDB FULLTEXT (5.6), and one
-- TIMESTAMP column per table with CURRENT_TIMESTAMP (5.6.5). Run by root.
SET NAMES utf8;

ALTER DATABASE kerno CHARACTER SET utf8;
CREATE DATABASE IF NOT EXISTS kerno_other CHARACTER SET utf8;

-- The test user administers the server in the admin tests.
GRANT ALL PRIVILEGES ON *.* TO 'kerno'@'%' WITH GRANT OPTION;

USE kerno;

CREATE TABLE customers (
    id         INT UNSIGNED NOT NULL AUTO_INCREMENT,
    email      VARCHAR(190) NOT NULL,
    full_name  VARCHAR(120) NOT NULL,
    is_active  TINYINT(1) NOT NULL DEFAULT 1,
    tier       ENUM('standard', 'vip', 'wholesale') NOT NULL DEFAULT 'standard',
    country    CHAR(2) NULL,
    created_at DATETIME NOT NULL,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    PRIMARY KEY (id),
    UNIQUE KEY customers_email_key (email),
    KEY customers_country_idx (country)
) ENGINE = InnoDB DEFAULT CHARSET = utf8 COMMENT = 'People who can place orders';

CREATE TABLE categories (
    id        INT NOT NULL AUTO_INCREMENT,
    parent_id INT NULL,
    name      VARCHAR(80) NOT NULL,
    PRIMARY KEY (id),
    CONSTRAINT categories_parent_fk FOREIGN KEY (parent_id) REFERENCES categories (id) ON DELETE SET NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8;

CREATE TABLE products (
    id          INT UNSIGNED NOT NULL AUTO_INCREMENT,
    category_id INT NOT NULL,
    sku         VARCHAR(32) NOT NULL,
    name        VARCHAR(160) NOT NULL COMMENT 'Display name',
    price       DECIMAL(10, 2) NOT NULL,
    stock       INT NOT NULL DEFAULT 0,
    released_on DATE NULL,
    PRIMARY KEY (id),
    UNIQUE KEY products_sku_key (sku),
    KEY products_category_idx (category_id),
    CONSTRAINT products_category_fk FOREIGN KEY (category_id) REFERENCES categories (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8;

CREATE TABLE orders (
    id          INT UNSIGNED NOT NULL AUTO_INCREMENT,
    customer_id INT UNSIGNED NOT NULL,
    status      ENUM('pending', 'paid', 'shipped', 'cancelled') NOT NULL DEFAULT 'pending',
    placed_at   DATETIME NOT NULL,
    total       DECIMAL(12, 2) NOT NULL DEFAULT 0,
    PRIMARY KEY (id),
    KEY orders_customer_idx (customer_id),
    CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES customers (id) ON DELETE RESTRICT
) ENGINE = InnoDB DEFAULT CHARSET = utf8;

CREATE TABLE order_items (
    order_id   INT UNSIGNED NOT NULL,
    line_no    SMALLINT NOT NULL,
    product_id INT UNSIGNED NOT NULL,
    quantity   INT NOT NULL,
    unit_price DECIMAL(10, 2) NOT NULL,
    PRIMARY KEY (order_id, line_no),
    KEY order_items_product_idx (product_id),
    CONSTRAINT order_items_order_fk FOREIGN KEY (order_id) REFERENCES orders (id) ON DELETE CASCADE,
    CONSTRAINT order_items_product_fk FOREIGN KEY (product_id) REFERENCES products (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8;

-- No primary key and no unique index on purpose.
CREATE TABLE audit_log (
    happened_at DATETIME NOT NULL,
    actor       VARCHAR(64) NOT NULL,
    action      VARCHAR(64) NOT NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8;

-- MyISAM: no transactions, and FULLTEXT on every version.
CREATE TABLE notes (
    id   INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    body TEXT NOT NULL,
    FULLTEXT KEY notes_body_ft (body)
) ENGINE = MyISAM DEFAULT CHARSET = utf8;

CREATE TABLE kerno_other.things (
    id   INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(40) NOT NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8;

-- One column per type every version has.
CREATE TABLE all_types (
    id          INT NOT NULL PRIMARY KEY,
    c_tinyint   TINYINT,
    c_bool      BOOLEAN,
    c_smallint  SMALLINT,
    c_mediumint MEDIUMINT,
    c_int_u     INT UNSIGNED,
    c_bigint    BIGINT,
    c_bigint_u  BIGINT UNSIGNED,
    c_decimal   DECIMAL(20, 6),
    c_float     FLOAT,
    c_double    DOUBLE,
    c_bit       BIT(8),
    c_char      CHAR(5),
    c_varchar   VARCHAR(50),
    c_text      TEXT,
    c_binary    BINARY(4),
    c_varbinary VARBINARY(16),
    c_blob      BLOB,
    c_date      DATE,
    c_time      TIME,
    c_datetime  DATETIME,
    c_timestamp TIMESTAMP NULL DEFAULT NULL,
    c_year      YEAR,
    c_enum      ENUM('sad', 'ok', 'happy'),
    c_set       SET('a', 'b', 'c')
) ENGINE = InnoDB DEFAULT CHARSET = utf8;

CREATE VIEW order_totals AS
SELECT o.id AS order_id,
       o.customer_id,
       o.status,
       COUNT(i.line_no) AS line_count,
       COALESCE(SUM(i.quantity * i.unit_price), 0) AS computed_total
FROM orders o
LEFT JOIN order_items i ON i.order_id = o.id
GROUP BY o.id, o.customer_id, o.status;

CREATE VIEW active_customers AS
SELECT id, email, full_name, tier FROM customers WHERE is_active = 1;

DELIMITER //

CREATE FUNCTION customer_lifetime_value(p_customer_id INT UNSIGNED)
RETURNS DECIMAL(14, 2)
READS SQL DATA
COMMENT 'Sum of the paid orders of a customer'
BEGIN
    DECLARE v_total DECIMAL(14, 2);
    SELECT COALESCE(SUM(total), 0) INTO v_total
    FROM orders
    WHERE customer_id = p_customer_id AND status IN ('paid', 'shipped');
    RETURN v_total;
END//

CREATE PROCEDURE customer_orders(IN p_customer_id INT UNSIGNED, OUT p_count INT)
READS SQL DATA
BEGIN
    SELECT COUNT(*) INTO p_count FROM orders WHERE customer_id = p_customer_id;
END//

CREATE TRIGGER orders_log_status
AFTER UPDATE ON orders
FOR EACH ROW
BEGIN
    IF NEW.status <> OLD.status THEN
        INSERT INTO audit_log (happened_at, actor, action)
        VALUES (NOW(), CURRENT_USER(), CONCAT('order ', NEW.id, ': ', OLD.status, ' -> ', NEW.status));
    END IF;
END//

-- An account whose password has the pre-4.1 hash (OLD_PASSWORD of
-- 'kerno_old_pw'), on the versions that still let one sign in (the server
-- refuses them from 5.6.5, and 5.7 has no such hash).
CREATE PROCEDURE kerno_seed_old_password()
BEGIN
    IF VERSION() LIKE '5.1.%' OR VERSION() LIKE '5.5.%' THEN
        CREATE USER 'kerno_old'@'%' IDENTIFIED BY PASSWORD '12b679f11d96f04d';
        GRANT SELECT ON kerno.* TO 'kerno_old'@'%';
    END IF;
END//

DELIMITER ;

CALL kerno_seed_old_password();
DROP PROCEDURE kerno_seed_old_password;

-- PRESERVE: 5.1 and 5.5 drop a NOT PRESERVE event when they restart (seen
-- on these containers, under emulation), and the image restarts after the seed.
CREATE EVENT purge_old_audit_log
    ON SCHEDULE EVERY 1 DAY STARTS '2030-01-01 03:00:00'
    ON COMPLETION PRESERVE
    DISABLE
    COMMENT 'Deletes audit entries older than a year'
    DO DELETE FROM audit_log WHERE happened_at < NOW() - INTERVAL 1 YEAR;

CREATE USER 'kerno_readonly'@'%' IDENTIFIED BY 'kerno_ro_pw';
GRANT SELECT ON kerno.* TO 'kerno_readonly'@'%';
GRANT SELECT, INSERT ON kerno.audit_log TO 'kerno_readonly'@'%';

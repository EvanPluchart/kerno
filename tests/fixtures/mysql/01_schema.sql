-- Kerno test fixture: MySQL / MariaDB sample schema (shared by both engines).
-- A small shop in database `kerno`, an analytics database and a table holding
-- one column per common type. Deterministic: no RAND(), no NOW() in the data.
SET NAMES utf8mb4;

CREATE DATABASE IF NOT EXISTS kerno_analytics CHARACTER SET utf8mb4;
CREATE DATABASE IF NOT EXISTS kerno_other CHARACTER SET utf8mb4;

-- The test user administers the server in the admin tests (process list,
-- KILL, users and grants, create/drop database).
GRANT ALL PRIVILEGES ON *.* TO 'kerno'@'%' WITH GRANT OPTION;

USE kerno;

-- Tables --------------------------------------------------------------------

CREATE TABLE customers (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    email       VARCHAR(190) NOT NULL,
    full_name   VARCHAR(120) NOT NULL,
    phone       VARCHAR(32) NULL,
    is_active   TINYINT(1) NOT NULL DEFAULT 1,
    tier        ENUM('standard', 'vip', 'wholesale') NOT NULL DEFAULT 'standard',
    channels    SET('email', 'sms', 'post') NOT NULL DEFAULT '',
    preferences JSON NULL,
    city        VARCHAR(80) NULL,
    country     CHAR(2) NULL,
    created_at  DATETIME NOT NULL,
    updated_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
    PRIMARY KEY (id),
    UNIQUE KEY customers_email_key (email),
    KEY customers_country_city_idx (country, city)
) ENGINE = InnoDB COMMENT = 'People who can place orders';

CREATE TABLE categories (
    id        INT NOT NULL AUTO_INCREMENT,
    parent_id INT NULL,
    name      VARCHAR(80) NOT NULL,
    slug      VARCHAR(80) NOT NULL,
    PRIMARY KEY (id),
    UNIQUE KEY categories_slug_key (slug),
    CONSTRAINT categories_parent_fk FOREIGN KEY (parent_id) REFERENCES categories (id) ON DELETE SET NULL
) ENGINE = InnoDB;

CREATE TABLE products (
    id           BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    public_id    CHAR(36) NOT NULL,
    category_id  INT NOT NULL,
    sku          VARCHAR(32) NOT NULL,
    name         VARCHAR(160) NOT NULL COMMENT 'Display name',
    description  TEXT NULL,
    price        DECIMAL(10, 2) NOT NULL,
    stock        INT NOT NULL DEFAULT 0,
    weight_kg    FLOAT NULL,
    attributes   JSON NULL,
    released_on  DATE NULL,
    discontinued TINYINT(1) NOT NULL DEFAULT 0,
    price_cents  BIGINT GENERATED ALWAYS AS (CAST(price * 100 AS SIGNED)) STORED,
    PRIMARY KEY (id),
    UNIQUE KEY products_sku_key (sku),
    KEY products_category_idx (category_id),
    FULLTEXT KEY products_name_ft (name),
    CONSTRAINT products_category_fk FOREIGN KEY (category_id) REFERENCES categories (id),
    CONSTRAINT products_price_check CHECK (price >= 0)
) ENGINE = InnoDB;

CREATE TABLE orders (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    customer_id BIGINT UNSIGNED NOT NULL,
    status      ENUM('pending', 'paid', 'shipped', 'delivered', 'cancelled', 'refunded') NOT NULL DEFAULT 'pending',
    placed_at   DATETIME NOT NULL,
    shipped_at  DATETIME NULL,
    total       DECIMAL(12, 2) NOT NULL DEFAULT 0,
    currency    CHAR(3) NOT NULL DEFAULT 'EUR',
    notes       TEXT NULL,
    PRIMARY KEY (id),
    KEY orders_customer_idx (customer_id),
    KEY orders_status_placed_idx (status, placed_at),
    CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES customers (id) ON DELETE RESTRICT
) ENGINE = InnoDB;

CREATE TABLE order_items (
    order_id   BIGINT UNSIGNED NOT NULL,
    line_no    SMALLINT NOT NULL,
    product_id BIGINT UNSIGNED NOT NULL,
    quantity   INT NOT NULL,
    unit_price DECIMAL(10, 2) NOT NULL,
    PRIMARY KEY (order_id, line_no),
    KEY order_items_product_idx (product_id),
    CONSTRAINT order_items_order_fk FOREIGN KEY (order_id) REFERENCES orders (id) ON DELETE CASCADE,
    CONSTRAINT order_items_product_fk FOREIGN KEY (product_id) REFERENCES products (id)
) ENGINE = InnoDB;

CREATE TABLE reviews (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    product_id  BIGINT UNSIGNED NOT NULL,
    customer_id BIGINT UNSIGNED NOT NULL,
    rating      TINYINT NOT NULL,
    title       VARCHAR(160) NULL,
    body        TEXT NULL,
    created_at  DATETIME NOT NULL,
    PRIMARY KEY (id),
    UNIQUE KEY reviews_once_per_customer (product_id, customer_id),
    CONSTRAINT reviews_product_fk FOREIGN KEY (product_id) REFERENCES products (id) ON DELETE CASCADE,
    CONSTRAINT reviews_customer_fk FOREIGN KEY (customer_id) REFERENCES customers (id) ON DELETE CASCADE,
    CONSTRAINT reviews_rating_check CHECK (rating BETWEEN 1 AND 5)
) ENGINE = InnoDB;

-- No primary key and no unique index on purpose: the data view must open
-- this table read-only and say why.
CREATE TABLE audit_log (
    happened_at DATETIME NOT NULL,
    actor       VARCHAR(64) NOT NULL,
    action      VARCHAR(64) NOT NULL,
    payload     JSON NULL
) ENGINE = InnoDB;

CREATE TABLE kerno_analytics.events (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    customer_id BIGINT UNSIGNED NULL,
    event_type  VARCHAR(40) NOT NULL,
    properties  JSON NULL,
    occurred_at DATETIME NOT NULL,
    PRIMARY KEY (id),
    KEY events_type_time_idx (event_type, occurred_at)
) ENGINE = InnoDB;

CREATE TABLE kerno_other.notes (
    id   INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    body TEXT NOT NULL
) ENGINE = InnoDB;

-- One column per common type -------------------------------------------------

CREATE TABLE all_types (
    id           INT NOT NULL PRIMARY KEY,
    c_tinyint    TINYINT,
    c_tinyint_u  TINYINT UNSIGNED,
    c_bool       BOOLEAN,
    c_smallint   SMALLINT,
    c_mediumint  MEDIUMINT,
    c_int        INT,
    c_int_u      INT UNSIGNED,
    c_bigint     BIGINT,
    c_bigint_u   BIGINT UNSIGNED,
    c_decimal    DECIMAL(20, 6),
    c_float      FLOAT,
    c_double     DOUBLE,
    c_bit        BIT(8),
    c_bit1       BIT(1),
    c_char       CHAR(5),
    c_varchar    VARCHAR(50),
    c_tinytext   TINYTEXT,
    c_text       TEXT,
    c_mediumtext MEDIUMTEXT,
    c_longtext   LONGTEXT,
    c_binary     BINARY(4),
    c_varbinary  VARBINARY(16),
    c_blob       BLOB,
    c_date       DATE,
    c_time       TIME(6),
    c_datetime   DATETIME(6),
    c_timestamp  TIMESTAMP(6) NULL,
    c_year       YEAR,
    c_enum       ENUM('sad', 'ok', 'happy'),
    c_set        SET('a', 'b', 'c'),
    c_json       JSON,
    c_point      POINT
) ENGINE = InnoDB;

-- Views ---------------------------------------------------------------------

CREATE VIEW order_totals AS
SELECT o.id AS order_id,
       o.customer_id,
       o.status,
       o.placed_at,
       COUNT(i.line_no) AS line_count,
       COALESCE(SUM(i.quantity * i.unit_price), 0) AS computed_total
FROM orders o
LEFT JOIN order_items i ON i.order_id = o.id
GROUP BY o.id, o.customer_id, o.status, o.placed_at;

CREATE VIEW active_customers AS
SELECT id, email, full_name, tier FROM customers WHERE is_active = 1;

-- Routines, triggers, events --------------------------------------------------

DELIMITER //

CREATE FUNCTION customer_lifetime_value(p_customer_id BIGINT UNSIGNED)
RETURNS DECIMAL(14, 2)
READS SQL DATA
COMMENT 'Sum of the paid orders of a customer'
BEGIN
    DECLARE v_total DECIMAL(14, 2);
    SELECT COALESCE(SUM(total), 0) INTO v_total
    FROM orders
    WHERE customer_id = p_customer_id
      AND status IN ('paid', 'shipped', 'delivered');
    RETURN v_total;
END//

CREATE PROCEDURE cancel_stale_orders(IN p_older_than_days INT)
MODIFIES SQL DATA
BEGIN
    UPDATE orders
    SET status = 'cancelled'
    WHERE status = 'pending'
      AND placed_at < NOW() - INTERVAL p_older_than_days DAY;
END//

CREATE PROCEDURE customer_orders(IN p_customer_id BIGINT UNSIGNED, OUT p_count INT)
READS SQL DATA
BEGIN
    SELECT COUNT(*) INTO p_count FROM orders WHERE customer_id = p_customer_id;
    SELECT id, status, total FROM orders WHERE customer_id = p_customer_id ORDER BY id;
END//

CREATE TRIGGER orders_log_status
AFTER UPDATE ON orders
FOR EACH ROW
BEGIN
    IF NEW.status <> OLD.status THEN
        INSERT INTO audit_log (happened_at, actor, action, payload)
        VALUES (NOW(), CURRENT_USER(), 'order_status',
                JSON_OBJECT('order_id', NEW.id, 'from', OLD.status, 'to', NEW.status));
    END IF;
END//

CREATE TRIGGER reviews_check_rating
BEFORE INSERT ON reviews
FOR EACH ROW
BEGIN
    IF NEW.rating < 1 OR NEW.rating > 5 THEN
        SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'rating must be between 1 and 5';
    END IF;
END//

DELIMITER ;

CREATE EVENT purge_old_audit_log
    ON SCHEDULE EVERY 1 DAY STARTS '2030-01-01 03:00:00'
    DISABLE
    COMMENT 'Deletes audit entries older than a year'
    DO DELETE FROM audit_log WHERE happened_at < NOW() - INTERVAL 1 YEAR;

-- Grants (read by the users/grants view) --------------------------------------

CREATE USER 'kerno_readonly'@'%' IDENTIFIED BY 'kerno_ro_pw';
GRANT SELECT ON kerno.* TO 'kerno_readonly'@'%';
GRANT SELECT, INSERT ON kerno.audit_log TO 'kerno_readonly'@'%';
-- Signs in with the client certificate the test CA issued, without a password.
CREATE USER 'kerno_cert'@'%' REQUIRE SUBJECT '/CN=kerno_cert';
GRANT SELECT ON kerno.* TO 'kerno_cert'@'%';

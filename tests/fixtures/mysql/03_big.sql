-- Kerno test fixture: 1,000,000 rows x 20 mixed columns, for the grid and
-- streaming benchmarks. Deterministic. Works on MySQL and MariaDB.
SET NAMES utf8mb4;
USE kerno_analytics;

CREATE TABLE big_table (
    id       BIGINT NOT NULL PRIMARY KEY,
    bucket   INT NOT NULL,
    small    SMALLINT NOT NULL,
    hash     CHAR(32) NOT NULL,
    username VARCHAR(32) NOT NULL,
    amount   DECIMAL(14, 2) NOT NULL,
    score    DOUBLE NOT NULL,
    ratio    FLOAT NOT NULL,
    is_even  TINYINT(1) NOT NULL,
    day      DATE NOT NULL,
    at_time  TIME NOT NULL,
    created  DATETIME NOT NULL,
    seen_at  TIMESTAMP NULL,
    note     VARCHAR(32) NULL,
    uid      CHAR(36) NOT NULL,
    doc      JSON NULL,
    flags    SET('a', 'b', 'c') NOT NULL,
    big_u    BIGINT UNSIGNED NOT NULL,
    mood     ENUM('sad', 'ok', 'happy') NOT NULL,
    padding  VARCHAR(40) NOT NULL,
    KEY big_table_bucket_idx (bucket)
) ENGINE = InnoDB;

INSERT INTO big_table
SELECT g,
       g % 1000,
       g % 32767,
       MD5(g),
       CONCAT('user_', g % 50000),
       g * 1.5,
       (g % 9973) / 7.0,
       g / 3.0,
       g % 2 = 0,
       DATE('2020-01-01') + INTERVAL (g % 2000) DAY,
       SEC_TO_TIME(g % 86400),
       TIMESTAMP('2020-01-01 00:00:00') + INTERVAL g SECOND,
       TIMESTAMP('2020-01-01 12:00:00') + INTERVAL g MINUTE,
       IF(g % 10 = 0, NULL, CONCAT('note ', g)),
       LOWER(CONCAT_WS('-', SUBSTR(MD5(CONCAT('uid', g)), 1, 8), SUBSTR(MD5(CONCAT('uid', g)), 9, 4),
                       SUBSTR(MD5(CONCAT('uid', g)), 13, 4), SUBSTR(MD5(CONCAT('uid', g)), 17, 4),
                       SUBSTR(MD5(CONCAT('uid', g)), 21, 12))),
       JSON_OBJECT('k', g % 100, 'tag', CONCAT('x', g % 7)),
       ELT(1 + g % 4, '', 'a', 'a,b', 'a,b,c'),
       18446744073709551615 - g,
       ELT(1 + g % 3, 'sad', 'ok', 'happy'),
       REPEAT(CHAR(97 + g % 26 USING utf8mb4), 1 + g % 40)
FROM kerno._seq
ORDER BY g;

ANALYZE TABLE big_table;

DROP TABLE kerno._seq;
DROP TABLE kerno._digits;

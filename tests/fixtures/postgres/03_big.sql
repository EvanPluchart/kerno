-- Kerno test fixture: 1,000,000 rows x 20 mixed columns, for the grid and
-- streaming benchmarks. Deterministic.

CREATE TABLE analytics.big_table AS
SELECT g::bigint                                                          AS id,
       (g % 1000)::integer                                                AS bucket,
       (g % 32767)::smallint                                              AS small,
       md5(g::text)                                                       AS hash,
       'user_' || (g % 50000)                                             AS username,
       (g * 1.5)::numeric(14, 2)                                          AS amount,
       ((g % 9973) / 7.0)::double precision                               AS score,
       (g / 3.0)::real                                                    AS ratio,
       g % 2 = 0                                                          AS is_even,
       DATE '2020-01-01' + (g % 2000)::integer                            AS day,
       TIME '00:00:00' + ((g % 86400) || ' seconds')::interval            AS at_time,
       TIMESTAMP '2020-01-01 00:00:00' + (g || ' seconds')::interval      AS created,
       TIMESTAMPTZ '2020-01-01 00:00:00+00' + (g || ' minutes')::interval AS seen_at,
       CASE WHEN g % 10 = 0 THEN NULL ELSE 'note ' || g END               AS note,
       md5('uid' || g)::uuid                                              AS uid,
       jsonb_build_object('k', g % 100, 'tag', 'x' || (g % 7))            AS doc,
       ARRAY[g % 3, g % 5, g % 7]::integer[]                              AS arr,
       ('10.' || (g % 256) || '.' || ((g / 256) % 256) || '.1')::inet     AS ip,
       (ARRAY['sad', 'ok', 'happy'])[1 + g % 3]::public.mood              AS mood,
       repeat(chr(97 + (g % 26)::integer), 1 + (g % 40)::integer)         AS padding
FROM generate_series(1, 1000000) g;

ALTER TABLE analytics.big_table ADD PRIMARY KEY (id);
CREATE INDEX big_table_bucket_idx ON analytics.big_table (bucket);
ANALYZE;

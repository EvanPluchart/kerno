-- Marker read by the container healthcheck: the seed is complete.
CREATE TABLE kerno.kerno_seed_done (done INT NOT NULL PRIMARY KEY);
INSERT INTO kerno.kerno_seed_done VALUES (1);

-- A second database, so the navigator has more than one to list.
CREATE DATABASE kerno_other;
\connect kerno_other
CREATE TABLE notes (id serial PRIMARY KEY, body text NOT NULL);
INSERT INTO notes (body) VALUES ('first'), ('second');

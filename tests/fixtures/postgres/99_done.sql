-- Marker read by the container healthcheck: the seed is complete.
\connect kerno
CREATE TABLE public.kerno_seed_done (done integer PRIMARY KEY);
INSERT INTO public.kerno_seed_done VALUES (1);

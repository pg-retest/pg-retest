-- Deterministic seed for the oracle-verified translation benchmark.
-- Idempotent: safe to run before every benchmark invocation.
DROP TABLE IF EXISTS oracle_users;
CREATE TABLE oracle_users (id int PRIMARY KEY, name text, active int);
INSERT INTO oracle_users (id, name, active) VALUES
  (1, 'alice', 1),
  (2, NULL,    0),
  (3, 'cara',  1);

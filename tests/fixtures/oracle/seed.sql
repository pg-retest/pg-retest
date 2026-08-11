-- Deterministic seed for the oracle-verified translation benchmarks (PostgreSQL).
-- Idempotent. Types kept to int/text/date so cross-engine canonicalization is exact
-- (float/decimal/timezone tolerance is a Phase-2e follow-on).
DROP TABLE IF EXISTS oracle_users;
CREATE TABLE oracle_users (id int PRIMARY KEY, name text, active int, created date, score int);
INSERT INTO oracle_users (id, name, active, created, score) VALUES
  (1, 'alice', 1, DATE '2024-01-15', 10),
  (2, NULL,    0, DATE '2024-02-20', 20),
  (3, 'cara',  1, DATE '2024-03-25', 30);

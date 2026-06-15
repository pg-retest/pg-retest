DROP TABLE IF EXISTS oracle_users;
CREATE TABLE oracle_users (id INT PRIMARY KEY, name VARCHAR(64), active INT);
INSERT INTO oracle_users (id, name, active) VALUES (1, 'alice', 1), (2, NULL, 0), (3, 'cara', 1);

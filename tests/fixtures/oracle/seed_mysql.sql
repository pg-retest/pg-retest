DROP TABLE IF EXISTS oracle_users;
CREATE TABLE oracle_users (id INT PRIMARY KEY, name VARCHAR(64), active INT, created DATE, score INT);
INSERT INTO oracle_users (id, name, active, created, score) VALUES
  (1, 'alice', 1, '2024-01-15', 10),
  (2, NULL, 0, '2024-02-20', 20),
  (3, 'cara', 1, '2024-03-25', 30);

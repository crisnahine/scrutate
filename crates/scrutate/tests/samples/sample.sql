-- SQL: heuristic symbols
CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
CREATE OR REPLACE FUNCTION add_one(i INT) RETURNS INT AS $$ SELECT i + 1 $$ LANGUAGE SQL;
create view active_users as select * from users where id > 0;

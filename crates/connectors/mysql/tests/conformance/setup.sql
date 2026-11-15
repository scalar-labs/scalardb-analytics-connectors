-- The MySQL fixture of the connector conformance suite. It runs as root and
-- replaces every object of an earlier run.

DROP DATABASE IF EXISTS conformance;
DROP DATABASE IF EXISTS conformance_other;
DROP DATABASE IF EXISTS Conformance;
DROP USER IF EXISTS conformance_reader, conformance_outsider;
CREATE USER conformance_reader IDENTIFIED BY 'conformance-reader-secret';
CREATE USER conformance_outsider IDENTIFIED BY 'conformance-outsider-secret';

CREATE DATABASE conformance CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci;
CREATE DATABASE conformance_other;
-- A database whose name differs only in case.
CREATE DATABASE Conformance;
USE conformance;

-- Metadata: every mapping rule, unmapped types, constraints, and names that
-- differ only in case.
CREATE TABLE parent (
  id bigint PRIMARY KEY,
  code varchar(20) NOT NULL,
  label varchar(40),
  UNIQUE KEY parent_code (code),
  UNIQUE KEY label_prefix (label(5)),
  UNIQUE KEY lower_code ((lower(code)))
);
CREATE TABLE `Mixed Case` (
  tiny tinyint,
  flag tinyint(1),
  small smallint NOT NULL,
  medium mediumint,
  regular int,
  big bigint,
  tiny_unsigned tinyint unsigned,
  small_unsigned smallint unsigned,
  medium_unsigned mediumint unsigned,
  regular_unsigned int unsigned,
  big_unsigned bigint unsigned,
  single float,
  double_value double,
  exact decimal(10, 2),
  widest decimal(38, 0),
  wide decimal(65, 30),
  fixed char(5),
  bounded varchar(10),
  tiny_text tinytext,
  free_text text,
  medium_text mediumtext,
  long_text longtext,
  fixed_bytes binary(3),
  bytes varbinary(10),
  tiny_blob tinyblob,
  regular_blob blob,
  medium_blob mediumblob,
  long_blob longblob,
  day date,
  stamp datetime(3),
  instant timestamp NULL,
  year_value year,
  feeling enum('sad', 'ok', 'happy'),
  document json,
  clock time,
  bits bit(3),
  bit_flag bit(1),
  tags set('a', 'b'),
  location point,
  parent_id bigint,
  CONSTRAINT positive_small CHECK (small > 0),
  CONSTRAINT loose CHECK (regular < 100) NOT ENFORCED,
  CONSTRAINT mixed_parent FOREIGN KEY (parent_id) REFERENCES parent (id)
);
CREATE TABLE `mixed case` (id int);
CREATE VIEW parent_view AS SELECT id FROM parent;
CREATE TABLE conformance_other.parent (id int);
CREATE TABLE Conformance.parent (id int);
-- Columns the outsider holds privileges on only one by one.
CREATE TABLE partial (id int NOT NULL, secret varchar(10));

-- Values: boundary values of every covered rule, and NULL.
CREATE TABLE value_types (
  id int NOT NULL,
  tiny tinyint,
  small smallint,
  medium mediumint,
  regular int,
  big bigint,
  tiny_unsigned tinyint unsigned,
  small_unsigned smallint unsigned,
  medium_unsigned mediumint unsigned,
  regular_unsigned int unsigned,
  single float,
  ratio double,
  narrow decimal(5, 2),
  exact decimal(38, 4),
  fixed char(5),
  bounded varchar(10),
  free_text text,
  fixed_bytes binary(3),
  bytes varbinary(10),
  day date,
  stamp datetime,
  precise datetime(6),
  instant timestamp(6) NULL,
  year_value year,
  feeling enum('sad', 'ok', 'happy'),
  document json,
  big_unsigned bigint unsigned,
  medium_exact decimal(18, 4),
  widest decimal(65, 30),
  flag bool,
  bit_flag bit(1)
);
INSERT INTO value_types VALUES
  (1, 127, 32767, 8388607, 2147483647, -9223372036854775808, 255, 65535, 16777215,
   4294967295, 0.5, 0.25, 999.99, 9999999999999999999999999999999999.9999, 'ab',
   'héllo', '日本語', x'00ff', x'00ff', '1000-01-01', '1000-01-01 00:00:00',
   '1970-01-01 00:00:00.000001', '1970-01-01 00:00:01.000000', 1901, 'ok', '{"a": 1}',
   18446744073709551615, 99999999999999.9999,
   99999999999999999999999999999999999.999999999999999999999999999999, TRUE, b'1'),
  (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
   NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
   NULL, NULL),
  (3, -128, -32768, -8388608, -2147483648, 9223372036854775807, 0, 0, 0, 0,
   -3.40282e38, 1.7976931348623157e308, -999.99, -9999999999999999999999999999999999.9999,
   'abcde', 'ten chars.', '', x'', x'', '9999-12-31', '9999-12-31 23:59:59',
   '9999-12-31 23:59:59.999999', '2038-01-19 03:14:07.999999', 2155, 'sad', '[]',
   0, -99999999999999.9999,
   -99999999999999999999999999999999999.999999999999999999999999999999, FALSE, b'0');

-- Values that cannot be carried exactly.
CREATE TABLE special (
  id int NOT NULL, day date, year_value year, flag bool, feeling enum('sad', 'ok', 'happy')
);
-- A non-strict sql_mode stores an invalid enum value as the error value ''.
SET SESSION sql_mode = '';
INSERT INTO special VALUES
  (1, '0000-00-00', 2000, TRUE, 'ok'), (2, '2020-01-01', 0, TRUE, 'ok'),
  (3, '2020-01-02', 2001, TRUE, 'ok'), (4, '2020-01-03', 2002, 2, 'ok'),
  (5, '2020-01-04', 2003, FALSE, 'bad');
SET SESSION sql_mode = DEFAULT;

-- Pushdown: values whose comparison meaning varies by column.
CREATE TABLE filtering (
  id int NOT NULL,
  label varchar(10) COLLATE utf8mb4_0900_bin,
  padded varchar(10) COLLATE utf8mb4_bin,
  folded varchar(10) COLLATE utf8mb4_0900_ai_ci,
  code char(3) COLLATE utf8mb4_0900_bin,
  score double,
  day date,
  amount decimal(5, 2),
  feeling enum('sad', 'ok', 'happy'),
  other_label varchar(10) COLLATE utf8mb4_0900_bin,
  counter bigint unsigned,
  huge decimal(65, 0),
  bit_flag bit(1)
);
INSERT INTO filtering VALUES
  (1, 'a', 'a', 'A', 'a', 0.5, '2020-01-01', 1.50, 'ok', 'a', 7, 1, b'1'),
  (2, 'b', 'b', 'b', 'b', 2.5, NULL, -1.00, 'sad', 'c', 9223372036854775808,
   99999999999999999999999999999999999999999999999999999999999999999, b'0'),
  (3, 'a ', 'a ', 'á', 'a', 1.5, '2021-01-01', NULL, 'happy', 'a ', 18446744073709551615,
   -99999999999999999999999999999999999999999999999999999999999999999, b'1');

-- Schema drift: one table per change.
CREATE TABLE retyped (id int NOT NULL, value int NOT NULL);
CREATE TABLE narrowed (id int NOT NULL, value int NOT NULL);
CREATE TABLE removed (id int NOT NULL);
CREATE TABLE relaxed (id int NOT NULL, value int NOT NULL);
CREATE TABLE tightened (id int NOT NULL, value int);
INSERT INTO retyped VALUES (1, 1);
INSERT INTO narrowed VALUES (1, 1);
INSERT INTO removed VALUES (1);
INSERT INTO relaxed VALUES (1, 1);
INSERT INTO tightened VALUES (1, 1);

-- Stopping: a read that waits, and one whose rows outlast the first batch.
CREATE VIEW slow AS SELECT SLEEP(30) AS n;
CREATE TABLE digits (n int NOT NULL);
INSERT INTO digits VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);
CREATE VIEW endless AS
  SELECT a.n + 10 * b.n + 100 * c.n + 1000 * d.n + 10000 * e.n + 100000 * f.n
       + 1000000 * g.n + 10000000 * h.n AS n
  FROM digits a, digits b, digits c, digits d, digits e, digits f, digits g, digits h;

-- Errors: a table the outsider may only write, and one with a column it
-- cannot see.
CREATE TABLE writable (id int NOT NULL, label varchar(10));
INSERT INTO writable VALUES (1, 'a');
CREATE TABLE hidden (id int NOT NULL, label varchar(10));
INSERT INTO hidden VALUES (1, 'a');

GRANT SELECT ON conformance.* TO conformance_reader;
GRANT SELECT ON conformance_other.* TO conformance_reader;
GRANT SELECT ON Conformance.* TO conformance_reader;
GRANT SELECT ON conformance.`Mixed Case` TO conformance_outsider;
GRANT SELECT ON conformance.parent TO conformance_outsider;
GRANT SELECT (id) ON conformance.partial TO conformance_outsider;
GRANT SELECT (id) ON conformance.hidden TO conformance_outsider;
GRANT INSERT ON conformance.writable TO conformance_outsider;

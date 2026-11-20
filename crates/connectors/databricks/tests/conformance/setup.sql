-- The fixture's objects. ${schema} and ${other} name the run's two schemas
-- in the fixture's catalog; statements end with a semicolon at the end of a
-- line. Each INSERT writes its rows in one statement, so that a read returns
-- them in that order.

CREATE SCHEMA ${schema};
CREATE SCHEMA ${other};

-- Every mapping rule, a collated string, and an unmapped type; read with its
-- boundary values and NULL.
CREATE TABLE ${schema}.value_types (
  id INT NOT NULL,
  tiny TINYINT,
  small SMALLINT,
  regular INT,
  big BIGINT,
  single FLOAT,
  ratio DOUBLE,
  narrow DECIMAL(5,2),
  medium DECIMAL(18,4),
  widest DECIMAL(38,4),
  label STRING,
  bounded VARCHAR(10),
  fixed CHAR(5),
  bytes BINARY,
  day DATE,
  stamp TIMESTAMP_NTZ,
  instant TIMESTAMP,
  flag BOOLEAN,
  tags ARRAY<INT>,
  attributes MAP<STRING, INT>,
  point STRUCT<x: INT, y: STRING>,
  folded STRING COLLATE UTF8_LCASE,
  document VARIANT
);
INSERT INTO ${schema}.value_types VALUES
  (1, 127, 32767, 2147483647, -9223372036854775808, CAST(0.5 AS FLOAT), 0.25D,
   999.99, 99999999999999.9999, 9999999999999999999999999999999999.9999,
   'héllo', 'ten chars.', 'ab', X'00FF00', DATE'0001-01-01',
   TIMESTAMP_NTZ'0001-01-01 00:00:00', TIMESTAMP'0001-01-01 00:00:00+00:00', true,
   ARRAY(1, NULL), MAP('a', 1), NAMED_STRUCT('x', 1, 'y', 'p'), 'A', PARSE_JSON('{"a": 1}')),
  (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
   NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
  (3, -128, -32768, -2147483648, 9223372036854775807, CAST(-3.4028235E38 AS FLOAT),
   1.7976931348623157E308D, -999.99, -99999999999999.9999,
   -9999999999999999999999999999999999.9999, '日本語', '', 'abcde', X'',
   DATE'9999-12-31', TIMESTAMP_NTZ'9999-12-31 23:59:59.999999',
   TIMESTAMP'9999-12-31 23:59:59.999999+00:00', false, CAST(ARRAY() AS ARRAY<INT>),
   CAST(MAP() AS MAP<STRING, INT>),
   NAMED_STRUCT('x', CAST(NULL AS INT), 'y', CAST(NULL AS STRING)), 'b', NULL);

-- Declared keys, which Unity Catalog does not enforce, and a view.
CREATE TABLE ${schema}.parent (
  id BIGINT NOT NULL,
  label STRING COMMENT 'Shown to people',
  CONSTRAINT parent_pk PRIMARY KEY (id)
) COMMENT 'Parents of the fixture';
CREATE TABLE ${schema}.child (
  id BIGINT NOT NULL,
  parent_id BIGINT,
  CONSTRAINT child_pk PRIMARY KEY (id),
  CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES ${schema}.parent (id)
);
CREATE VIEW ${schema}.parent_view AS SELECT id FROM ${schema}.parent;
CREATE TABLE ${other}.parent (id INT);

-- Filters: text under UTF8_BINARY, under UTF8_LCASE, and in a char column,
-- and the other comparable types.
CREATE TABLE ${schema}.filtering (
  id INT NOT NULL,
  label STRING,
  folded STRING COLLATE UTF8_LCASE,
  code CHAR(3),
  score DOUBLE,
  day DATE,
  amount DECIMAL(5,2),
  instant TIMESTAMP,
  stamp TIMESTAMP_NTZ,
  flag BOOLEAN,
  other_label STRING
);
INSERT INTO ${schema}.filtering VALUES
  (1, 'a', 'A', 'a', 0.5D, DATE'2020-01-01', 1.50, TIMESTAMP'2020-01-01 00:00:00+00:00',
   TIMESTAMP_NTZ'2020-01-01 00:00:00', true, 'a'),
  (2, 'b', 'b', 'b', 2.5D, NULL, -1.00, TIMESTAMP'2020-01-01 09:00:00+09:00',
   TIMESTAMP_NTZ'2020-06-01 00:00:00', false, 'c'),
  (3, 'a ', 'a', 'a', 1.5D, DATE'2021-01-01', NULL, TIMESTAMP'2021-01-01 00:00:00+00:00',
   NULL, true, 'a ');

-- Tables the drift cases change, one each.
CREATE TABLE ${schema}.retyped (id INT NOT NULL, value INT NOT NULL);
CREATE TABLE ${schema}.narrowed (id INT NOT NULL, value INT NOT NULL);
CREATE TABLE ${schema}.removed (id INT NOT NULL);
CREATE TABLE ${schema}.relaxed (id INT NOT NULL, value INT NOT NULL);
CREATE TABLE ${schema}.tightened (id INT NOT NULL, value INT);
INSERT INTO ${schema}.tightened VALUES (1, 1);

-- The waiting read, which counts in one task for minutes before it returns
-- its one row, and the long read, whose rows the warehouse keeps producing.
CREATE VIEW ${schema}.slow AS
  SELECT CAST(count(*) AS BIGINT) AS n FROM range(0, 100000000000, 1, 1) WHERE hash(id) = 0;
CREATE VIEW ${schema}.endless AS SELECT id AS n FROM range(0, 100000000000);

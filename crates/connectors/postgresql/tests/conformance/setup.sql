-- The PostgreSQL fixture of the connector conformance suite. It runs as the
-- superuser in one transaction and replaces every object of an earlier run.

DROP SCHEMA IF EXISTS conformance CASCADE;
DROP SCHEMA IF EXISTS conformance_other CASCADE;
DROP ROLE IF EXISTS conformance_reader;
DROP ROLE IF EXISTS conformance_outsider;
CREATE ROLE conformance_reader LOGIN PASSWORD 'conformance-reader-secret';
CREATE ROLE conformance_outsider LOGIN PASSWORD 'conformance-outsider-secret';

CREATE SCHEMA conformance;
CREATE SCHEMA conformance_other;
SET search_path = conformance;

-- Metadata: every mapping rule, unmapped types, constraints, and names that
-- differ only in case.
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
CREATE DOMAIN positive AS integer CHECK (VALUE > 0);
CREATE DOMAIN amount AS numeric(12, 2);
CREATE DOMAIN nested_amount AS amount;
CREATE DOMAIN required_integer AS integer NOT NULL;
CREATE DOMAIN nested_required AS required_integer;
CREATE TABLE strict (direct required_integer, nested nested_required, plain integer);
-- An invalid unique index, as a failed `CREATE UNIQUE INDEX CONCURRENTLY`
-- leaves it: the build cannot run in this transaction, so a valid index is
-- marked invalid in the catalog instead.
CREATE TABLE pending (code text);
CREATE UNIQUE INDEX pending_code ON pending (code);
UPDATE pg_catalog.pg_index SET indisvalid = false
  WHERE indexrelid = 'pending_code'::regclass;
CREATE TABLE parent (id bigint PRIMARY KEY, code text NOT NULL UNIQUE);
CREATE TABLE "Mixed Case" (
  flag boolean,
  small smallint NOT NULL,
  regular integer,
  big bigint,
  real_value real,
  single real,
  double_value double precision,
  exact numeric(10, 2),
  widest numeric(38, 0),
  too_wide numeric(39, 0),
  unbounded numeric,
  negative_scale numeric(5, -2),
  free_text text,
  bounded varchar(10),
  padded char(5),
  bytes bytea,
  day date,
  clock time,
  stamp timestamp(3),
  instant timestamptz,
  span interval,
  id uuid,
  document json,
  binary_document jsonb,
  feeling mood,
  zoned timetz,
  count positive,
  money_amount nested_amount,
  numbers integer[],
  money_value money,
  parent_id bigint REFERENCES parent (id),
  CONSTRAINT positive_small CHECK (small > 0)
);
ALTER TABLE "Mixed Case" DROP COLUMN real_value;
CREATE TABLE "mixed case" (id integer);
CREATE UNIQUE INDEX active_code ON parent (code) WHERE id > 0;
CREATE UNIQUE INDEX lower_code ON parent (lower(code));
CREATE VIEW parent_view AS SELECT id FROM parent;
CREATE TABLE conformance_other.parent (id integer);

-- Values: boundary values of every covered rule, and NULL.
CREATE TABLE value_types (
  id integer NOT NULL,
  flag boolean,
  small smallint,
  regular integer,
  big bigint,
  single real,
  ratio double precision,
  exact numeric(38, 4),
  free_text text,
  bounded varchar(10),
  padded char(5),
  bytes bytea,
  day date,
  clock time,
  stamp timestamp,
  instant timestamptz,
  span interval,
  external_id uuid,
  document json,
  binary_document jsonb,
  feeling mood,
  zoned timetz,
  count positive
);
INSERT INTO value_types VALUES
  (1, true, 32767, 2147483647, -9223372036854775808, 0.5, 0.25,
   '9999999999999999999999999999999999.9999', 'héllo', 'bounded', 'ab', '\x00ff',
   '0001-01-01 BC', '23:59:59.999999', '1970-01-01 00:00:00.000001',
   '2020-01-01 00:00:00+09', '1 year 2 mons 3 days 04:05:06.789',
   'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', '{"a": 1}', '{"b": [1, 2]}', 'ok',
   '10:00:00.25+05:30', 1),
  (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
   NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
  (3, false, -32768, -2147483648, 9223372036854775807, -3.4028235e38,
   1.7976931348623157e308, '-9999999999999999999999999999999999.9999', '',
   'ten chars.', 'abcde', '\x', '5874897-12-31', '00:00:00',
   '294247-01-10 04:00:54.775807', '2020-01-01 00:00:00-08',
   '-1 year -2 days -00:00:01', '00000000-0000-0000-0000-000000000000', '[]',
   '{}', 'sad', '00:00:00+00', 2147483647);

-- Values that cannot be carried exactly.
CREATE TABLE special (id integer NOT NULL, amount numeric(10, 2), clock time);
INSERT INTO special VALUES (1, 'NaN', '12:00'), (2, 1.50, '24:00:00'), (3, -0.01, '00:00');
CREATE TABLE infinite (id integer NOT NULL, day date, stamp timestamp, instant timestamptz);
INSERT INTO infinite VALUES
  (1, 'infinity', 'infinity', 'infinity'),
  (2, '-infinity', '-infinity', '-infinity');
CREATE TABLE beyond (id integer NOT NULL, stamp timestamp);
INSERT INTO beyond VALUES (1, '294276-12-31 23:59:59.999999');
CREATE TABLE long_span (id integer NOT NULL, span interval);
INSERT INTO long_span VALUES (1, '2562048 hours');

-- Pushdown: values whose comparison meaning varies by column.
CREATE COLLATION case_insensitive (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
CREATE TABLE filtering (
  id integer NOT NULL,
  label text,
  code char(3),
  folded text COLLATE case_insensitive,
  score double precision,
  external_id uuid,
  c_label text COLLATE "C",
  posix_label text COLLATE "POSIX",
  day date
);
INSERT INTO filtering VALUES
  (1, 'a', 'a', 'A', 0.5, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 'a', 'a', '2020-01-01'),
  (2, 'b', 'b', 'b', 'NaN', NULL, 'b', 'c', NULL),
  (3, 'a ', 'a', 'a', 1.5, NULL, 'a', 'b', '2021-01-01');

-- Schema drift: one table per change.
CREATE TABLE retyped (id integer NOT NULL, value integer NOT NULL);
CREATE TABLE narrowed (id integer NOT NULL, value integer NOT NULL);
CREATE TABLE removed (id integer NOT NULL);
CREATE TABLE relaxed (id integer NOT NULL, value integer NOT NULL);
CREATE TABLE tightened (id integer NOT NULL, value integer);
INSERT INTO retyped VALUES (1, 1);
INSERT INTO narrowed VALUES (1, 1);
INSERT INTO removed VALUES (1);
INSERT INTO relaxed VALUES (1, 1);
INSERT INTO tightened VALUES (1, 1);

-- Stopping: a read that waits, and one whose rows outlast the first batch.
CREATE FUNCTION slow_value() RETURNS integer LANGUAGE sql AS 'SELECT 1 FROM pg_sleep(30)';
CREATE VIEW slow AS SELECT slow_value() AS n;
CREATE VIEW endless AS
  SELECT a.n * 1000 + b.n AS n
  FROM generate_series(1, 1000000) AS a (n) CROSS JOIN generate_series(1, 1000) AS b (n);

-- Errors: a table the outsider may not read.
CREATE TABLE hidden (id integer NOT NULL, label text);
INSERT INTO hidden VALUES (1, 'a');

GRANT USAGE ON SCHEMA conformance, conformance_other TO conformance_reader, conformance_outsider;
GRANT SELECT ON ALL TABLES IN SCHEMA conformance, conformance_other TO conformance_reader;
GRANT SELECT ON conformance."Mixed Case", conformance.parent TO conformance_outsider;

-- The conformance fixture: its users, their privileges, and every object the
-- cases refer to, recreated on each run. SQL*Plus runs it as SYSTEM in the
-- pluggable database.

-- Sessions of an earlier run would keep its users from being dropped. A
-- session that ended meanwhile (ORA-00030) or that Oracle marks to end once
-- it can (ORA-00031) is ended.
BEGIN
    FOR s IN (SELECT sid, serial# AS serial FROM v$session
              WHERE username IN ('CONFORMANCE_READER', 'CONFORMANCE_OUTSIDER')) LOOP
        BEGIN
            EXECUTE IMMEDIATE 'ALTER SYSTEM KILL SESSION ''' || s.sid || ',' || s.serial
                || ''' IMMEDIATE';
        EXCEPTION WHEN OTHERS THEN
            IF SQLCODE NOT IN (-30, -31) THEN RAISE; END IF;
        END;
    END LOOP;
END;
/
DROP USER IF EXISTS conformance CASCADE;
DROP USER IF EXISTS conformance_other CASCADE;
DROP USER IF EXISTS conformance_reader CASCADE;
DROP USER IF EXISTS conformance_outsider CASCADE;

-- The schemas the fixture's objects belong to, which no one logs in to.
CREATE USER conformance NO AUTHENTICATION QUOTA UNLIMITED ON users;
CREATE USER conformance_other NO AUTHENTICATION QUOTA UNLIMITED ON users;
-- The reader sees and reads every table.
CREATE USER conformance_reader IDENTIFIED BY "Conformance-reader-secret1";
GRANT CREATE SESSION, SELECT ANY TABLE TO conformance_reader;
-- The outsider sees only the tables it is granted a privilege on. Its
-- password has characters the connection string grammar gives meaning.
CREATE USER conformance_outsider IDENTIFIED BY "Conformance={outsider} secret1";
GRANT CREATE SESSION TO conformance_outsider;

-- Every covered mapping rule: maxima, NULL, and minima.
CREATE TABLE conformance.value_types (
    id NUMBER(10) NOT NULL CONSTRAINT value_types_pkey PRIMARY KEY,
    whole NUMBER(38),
    fraction NUMBER(38, 38),
    small NUMBER(5, 2),
    middle NUMBER(18, 4),
    single BINARY_FLOAT,
    ratio BINARY_DOUBLE,
    flag BOOLEAN,
    fixed CHAR(5),
    unicode_fixed NCHAR(5),
    bounded VARCHAR2(10 CHAR),
    wide NVARCHAR2(20),
    bytes RAW(2),
    day DATE,
    stamp TIMESTAMP,
    stamp3 TIMESTAMP(3),
    stamp0 TIMESTAMP(0),
    instant TIMESTAMP WITH TIME ZONE,
    local_instant TIMESTAMP WITH LOCAL TIME ZONE
);
INSERT INTO conformance.value_types VALUES
    (1, 99999999999999999999999999999999999999, 0.99999999999999999999999999999999999999,
     999.99, 99999999999999.9999, 3.40282347E+38F, BINARY_DOUBLE_INFINITY, TRUE,
     'ab', N'é', 'bounded', N'héllo 😀', HEXTORAW('00FF'),
     TO_DATE('9999-12-31 23:59:59', 'YYYY-MM-DD HH24:MI:SS'),
     TIMESTAMP '9999-12-31 23:59:59.999999', TIMESTAMP '9999-12-31 23:59:59.999',
     TIMESTAMP '9999-12-31 23:59:59', TIMESTAMP '2020-01-01 09:00:00.000001 +09:00',
     TIMESTAMP '2020-01-01 09:00:00.000001 +09:00'),
    (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
     NULL, NULL, NULL, NULL),
    (3, -99999999999999999999999999999999999999, -0.99999999999999999999999999999999999999,
     -999.99, -99999999999999.9999, -3.40282347E+38F, -1.7976931348623157E+308D, FALSE,
     'abcde', N'abcde', 'a', N'a', HEXTORAW('0102'),
     TO_DATE('-4712-01-01', 'SYYYY-MM-DD'),
     TO_TIMESTAMP('-4712-01-01 00:00:00', 'SYYYY-MM-DD HH24:MI:SS'),
     TO_TIMESTAMP('-4712-01-01 00:00:00', 'SYYYY-MM-DD HH24:MI:SS'),
     TO_TIMESTAMP('-4712-01-01 00:00:00', 'SYYYY-MM-DD HH24:MI:SS'),
     TIMESTAMP '2020-07-01 12:00:00 America/New_York',
     TIMESTAMP '0001-01-01 00:00:00 +00:00');

-- Names that differ only in case, and one beyond ASCII.
CREATE TABLE conformance."Mixed Case" (id NUMBER(10) NOT NULL);
CREATE TABLE conformance."mixed case" (id NUMBER(10) NOT NULL);
CREATE TABLE conformance."Ünï😀" ("çol" NUMBER(10) NOT NULL);
INSERT INTO conformance."Ünï😀" VALUES (7);

-- Every kind of constraint, column expressions, comments, and the types the
-- mapping does not map or this version does not cover.
CREATE TYPE conformance.address AS OBJECT (street VARCHAR2(20));
/
CREATE TABLE conformance.parent (
    id NUMBER(10) GENERATED ALWAYS AS IDENTITY CONSTRAINT parent_pkey PRIMARY KEY,
    code VARCHAR2(10 CHAR) CONSTRAINT parent_code_nn NOT NULL
        CONSTRAINT parent_code_key UNIQUE,
    amount NUMBER(10, 2) DEFAULT 0 NOT NULL,
    doubled NUMBER(12, 2) GENERATED ALWAYS AS (amount * 2) VIRTUAL,
    slot NUMBER(10),
    note CLOB,
    unicode_note NCLOB,
    payload BLOB,
    anything NUMBER,
    whole INTEGER,
    rounded NUMBER(5, -2),
    tiny NUMBER(2, 5),
    approximate FLOAT(10),
    precise TIMESTAMP(9),
    precise_instant TIMESTAMP(9) WITH TIME ZONE,
    span_months INTERVAL YEAR TO MONTH,
    span_seconds INTERVAL DAY TO SECOND,
    legacy LONG,
    row_address ROWID,
    any_row_address UROWID,
    external_file BFILE,
    document JSON,
    embedding VECTOR(3, FLOAT32),
    place conformance.address
);
CREATE UNIQUE INDEX conformance.parent_slot_key ON conformance.parent (slot);
-- A CHECK condition longer than Oracle's 4000-byte copy of it.
DECLARE
    condition VARCHAR2(32767) := 'amount >= 0';
BEGIN
    FOR n IN 1 .. 500 LOOP
        condition := condition || ' AND amount <> ' || n;
    END LOOP;
    EXECUTE IMMEDIATE 'ALTER TABLE conformance.parent ADD CONSTRAINT parent_amount_check CHECK ('
        || condition || ')';
END;
/
COMMENT ON TABLE conformance.parent IS 'Parents 😀';
COMMENT ON COLUMN conformance.parent.code IS 'The parent''s code';

CREATE TABLE conformance_other.parent (
    id NUMBER(10) NOT NULL CONSTRAINT other_parent_pkey PRIMARY KEY
);
GRANT REFERENCES ON conformance_other.parent TO conformance;
CREATE TABLE conformance.child (
    id NUMBER(10) NOT NULL CONSTRAINT child_pkey PRIMARY KEY,
    parent_id NUMBER(10) CONSTRAINT child_parent_fkey REFERENCES conformance.parent (id),
    other_id NUMBER(10)
);
-- A foreign key that is neither enforced nor validated.
ALTER TABLE conformance.child ADD CONSTRAINT child_other_fkey FOREIGN KEY (other_id)
    REFERENCES conformance_other.parent (id) DISABLE NOVALIDATE;
CREATE VIEW conformance.parent_view AS SELECT id, code FROM conformance.parent;

-- NOT NULL conditions that leave their columns nullable, and unique indexes
-- on a virtual column and on an expression.
CREATE TABLE conformance.conditions (
    id NUMBER(10) NOT NULL,
    checked NUMBER(10) CONSTRAINT conditions_checked_check CHECK ("CHECKED" IS NOT NULL),
    unvalidated NUMBER(10),
    disabled NUMBER(10) CONSTRAINT conditions_disabled_nn NOT NULL DISABLE,
    doubled NUMBER(12) GENERATED ALWAYS AS (id * 2) VIRTUAL
);
INSERT INTO conformance.conditions (id, checked, unvalidated) VALUES (1, 1, NULL);
ALTER TABLE conformance.conditions MODIFY unvalidated
    CONSTRAINT conditions_unvalidated_nn NOT NULL ENABLE NOVALIDATE;
CREATE UNIQUE INDEX conformance.conditions_doubled_key ON conformance.conditions (doubled);
CREATE UNIQUE INDEX conformance.conditions_id_key ON conformance.conditions (id DESC);

-- Tables the outsider may see without reading them, or not at all.
CREATE TABLE conformance.hidden (
    id NUMBER(10) NOT NULL CONSTRAINT hidden_pkey PRIMARY KEY,
    label NVARCHAR2(20)
);
INSERT INTO conformance.hidden VALUES (1, N'conformance-literal');
GRANT INSERT ON conformance.hidden TO conformance_outsider;
CREATE TABLE conformance.invisible (id NUMBER(10) NOT NULL);
-- The outsider sees the child but not the parents its foreign keys reference.
GRANT SELECT ON conformance.child TO conformance_outsider;

-- Filters that run at Oracle or stay local.
CREATE TABLE conformance.filtering (
    id NUMBER(10) NOT NULL CONSTRAINT filtering_pkey PRIMARY KEY,
    label NVARCHAR2(10),
    padded CHAR(3),
    score BINARY_DOUBLE,
    amount NUMBER(10, 2),
    stamp TIMESTAMP,
    day DATE,
    instant TIMESTAMP WITH TIME ZONE,
    flag BOOLEAN
);
INSERT INTO conformance.filtering VALUES
    (1, N'a', 'a', 0.5, 1.50, TIMESTAMP '2020-01-01 00:00:00', DATE '2020-01-01',
     TIMESTAMP '2020-01-01 09:00:00 +09:00', TRUE),
    (2, N'b', 'b', NULL, 2.50, TIMESTAMP '2021-01-01 00:00:00', NULL,
     TIMESTAMP '2021-01-01 00:00:00 +00:00', FALSE),
    (3, N'a ', 'a', 1.5, -1.00, NULL, DATE '2021-01-01', NULL, NULL);

-- Source changes between planning and reading.
CREATE TABLE conformance.retyped (id NUMBER(10) NOT NULL, value NUMBER(10) NOT NULL);
CREATE TABLE conformance.narrowed (id NUMBER(10) NOT NULL, value NUMBER(10) NOT NULL);
CREATE TABLE conformance.removed (id NUMBER(10) NOT NULL);
CREATE TABLE conformance.relaxed (id NUMBER(10) NOT NULL, value NUMBER(10) NOT NULL);
CREATE TABLE conformance.tightened (id NUMBER(10) NOT NULL, value NUMBER(10));
INSERT INTO conformance.retyped VALUES (1, 1);
INSERT INTO conformance.narrowed VALUES (1, 1);
INSERT INTO conformance.removed VALUES (1);
INSERT INTO conformance.relaxed VALUES (1, 1);
INSERT INTO conformance.tightened VALUES (1, 1);

-- A read whose only row arrives after a long sum, and one that streams rows
-- for long.
CREATE VIEW conformance.slow AS
    SELECT CAST(MOD(SUM(a.n * b.n), 2) AS NUMBER(10)) AS n
    FROM (SELECT LEVEL AS n FROM dual CONNECT BY LEVEL <= 100000) a
    CROSS JOIN (SELECT LEVEL AS n FROM dual CONNECT BY LEVEL <= 100000) b;
CREATE VIEW conformance.endless AS
    SELECT CAST(1 AS NUMBER(10)) AS n
    FROM (SELECT LEVEL AS n FROM dual CONNECT BY LEVEL <= 100000) a
    CROSS JOIN (SELECT LEVEL AS n FROM dual CONNECT BY LEVEL <= 100000) b;
COMMIT;

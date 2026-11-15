-- The conformance fixture: the database, its logins, and every object the
-- cases refer to, recreated on each run.

IF DB_ID(N'scalardb_conformance') IS NOT NULL
BEGIN
    ALTER DATABASE scalardb_conformance SET SINGLE_USER WITH ROLLBACK IMMEDIATE;
    DROP DATABASE scalardb_conformance;
END
GO
-- A case-sensitive collation, so that names may differ only in case.
CREATE DATABASE scalardb_conformance COLLATE Latin1_General_100_CS_AS;
GO
IF SUSER_ID(N'conformance_reader') IS NULL
    CREATE LOGIN conformance_reader
        WITH PASSWORD = N'Conformance-reader-secret1', CHECK_POLICY = OFF;
IF SUSER_ID(N'conformance_outsider') IS NULL
    CREATE LOGIN conformance_outsider
        WITH PASSWORD = N'Conformance-outsider-secret1', CHECK_POLICY = OFF;
IF SUSER_ID(N'conformance_denied') IS NULL
    CREATE LOGIN conformance_denied
        WITH PASSWORD = N'Conformance-denied-secret1', CHECK_POLICY = OFF;
GO
USE scalardb_conformance;
GO
CREATE USER conformance_reader FOR LOGIN conformance_reader;
CREATE USER conformance_outsider FOR LOGIN conformance_outsider;
CREATE USER conformance_denied FOR LOGIN conformance_denied;
-- The reader sees every object's metadata and reads every table.
GRANT VIEW DEFINITION TO conformance_reader;
GRANT SELECT TO conformance_reader;
-- This login sees every object's metadata but one schema's.
GRANT VIEW DEFINITION TO conformance_denied;
GO
CREATE SCHEMA conformance;
GO
CREATE SCHEMA conformance_other;
GO
CREATE TYPE conformance.code FROM nvarchar(10);
GO

-- Every covered mapping rule: maxima, NULL, and minima.
CREATE TABLE conformance.value_types (
    id int NOT NULL CONSTRAINT value_types_pkey PRIMARY KEY,
    flag bit,
    tiny tinyint,
    small smallint,
    regular int,
    big bigint,
    single real,
    ratio float,
    exact decimal(38, 4),
    narrow numeric(9, 2),
    cash money,
    small_cash smallmoney,
    fixed char(5),
    bounded varchar(10),
    unicode_fixed nchar(5),
    wide nvarchar(20),
    bytes binary(2),
    varbytes varbinary(10),
    day date,
    clock time(7),
    stamp datetime2(7),
    stamp3 datetime2(3),
    legacy datetime,
    small_stamp smalldatetime,
    instant datetimeoffset(7),
    external_id uniqueidentifier,
    code conformance.code,
    label sysname NULL
);
INSERT INTO conformance.value_types VALUES
    (1, 1, 255, 32767, 2147483647, -9223372036854775808, 0.5, 0.25,
     9999999999999999999999999999999999.9999, 9999999.99,
     922337203685477.5807, 214748.3647,
     'ab', 'bounded', N'é', N'héllo 😀', 0x00FF, 0x00FF,
     '9999-12-31', '23:59:59.9999990', '9999-12-31 23:59:59.9999990',
     '9999-12-31 23:59:59.999', '9999-12-31 23:59:59.997', '2079-06-06 23:59:00',
     '2020-01-01 09:00:00.0000010 +09:00', 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11',
     N'X1', N'name'),
    (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
     NULL, NULL, NULL, NULL),
    (3, 0, 0, -32768, -2147483648, 9223372036854775807, -3.4028235E+38,
     1.7976931348623157E+308,
     -9999999999999999999999999999999999.9999, -9999999.99,
     -922337203685477.5808, -214748.3648,
     'abcde', '', N'abcde', N'', 0x0102, 0x,
     '0001-01-01', '00:00:00', '0001-01-01 00:00:00',
     '0001-01-01 00:00:00', '1753-01-01 00:00:00', '1900-01-01 00:00:00',
     '0001-01-01 00:00:00 +00:00', '00000000-0000-0000-0000-000000000000',
     N'', N'');

-- Times with a fraction of a microsecond, which cannot be carried.
CREATE TABLE conformance.fractions (
    id int NOT NULL CONSTRAINT fractions_pkey PRIMARY KEY,
    clock time(7),
    stamp datetime2(7),
    instant datetimeoffset(7)
);
INSERT INTO conformance.fractions VALUES
    (1, '00:00:00.0000001', '2020-01-01 00:00:00.0000001',
     '2020-01-01 00:00:00.0000001 +00:00'),
    (2, '00:00:00.0000010', '2020-01-01 00:00:00.0000010',
     '2020-01-01 00:00:00.0000010 +00:00');

-- Names that differ only in case.
CREATE TABLE conformance.[Mixed Case] (id int NOT NULL);
CREATE TABLE conformance.[mixed case] (id int NOT NULL);

-- Every kind of constraint, column expressions, comments, and the types the
-- mapping does not map or this version does not cover.
CREATE TABLE conformance.parent (
    id int IDENTITY(1, 1) NOT NULL CONSTRAINT parent_pkey PRIMARY KEY,
    code conformance.code NOT NULL CONSTRAINT parent_code_key UNIQUE,
    amount decimal(10, 2) NOT NULL CONSTRAINT parent_amount_default DEFAULT (0),
    doubled AS (amount * 2) PERSISTED,
    tripled AS (amount * 3),
    deleted_at datetime2(7),
    slot int,
    note nvarchar(max),
    payload varbinary(max),
    old_text text,
    old_ntext ntext,
    picture image,
    document xml,
    anything sql_variant,
    node hierarchyid,
    place geography,
    version rowversion NOT NULL,
    name sysname NULL
);
CREATE UNIQUE INDEX parent_slot_key ON conformance.parent (slot) WHERE deleted_at IS NULL;
GO
-- A CHECK expression longer than one 8000-byte piece.
DECLARE @check nvarchar(max) =
    N'ALTER TABLE conformance.parent ADD CONSTRAINT parent_amount_check CHECK (amount >= 0';
SELECT @check += N' AND amount <> ' + CAST(n AS nvarchar(10))
FROM (SELECT TOP (500) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS n FROM sys.all_objects) numbers;
SET @check += N')';
EXEC (@check);
GO
EXEC sp_addextendedproperty N'MS_Description', N'Parents 😀',
    N'SCHEMA', N'conformance', N'TABLE', N'parent';
EXEC sp_addextendedproperty N'MS_Description', N'The parent''s code',
    N'SCHEMA', N'conformance', N'TABLE', N'parent', N'COLUMN', N'code';
GO

CREATE TABLE conformance_other.parent (
    id int NOT NULL CONSTRAINT other_parent_pkey PRIMARY KEY
);
CREATE TABLE conformance.child (
    id int NOT NULL CONSTRAINT child_pkey PRIMARY KEY,
    parent_id int CONSTRAINT child_parent_fkey REFERENCES conformance.parent (id),
    other_id int
);
-- A foreign key that is neither enforced nor validated.
ALTER TABLE conformance.child WITH NOCHECK
    ADD CONSTRAINT child_other_fkey FOREIGN KEY (other_id) REFERENCES conformance_other.parent (id);
ALTER TABLE conformance.child NOCHECK CONSTRAINT child_other_fkey;
GO
CREATE VIEW conformance.parent_view AS SELECT id, code FROM conformance.parent;
GO
CREATE VIEW conformance.secret_view WITH ENCRYPTION AS SELECT id FROM conformance.parent;
GO

-- Tables the outsider may see only in part, or not at all.
CREATE TABLE conformance.partial (
    id int NOT NULL CONSTRAINT partial_pkey PRIMARY KEY,
    secret int
);
GRANT SELECT (id) ON conformance.partial TO conformance_outsider;
CREATE TABLE conformance.hidden (
    id int NOT NULL CONSTRAINT hidden_pkey PRIMARY KEY,
    label nvarchar(20)
);
INSERT INTO conformance.hidden VALUES (1, N'conformance-literal');
GRANT VIEW DEFINITION ON conformance.hidden TO conformance_outsider;
CREATE TABLE conformance.invisible (id int NOT NULL);
-- A column whose alias type the outsider has no permission on.
CREATE TABLE conformance.aliased (
    id int NOT NULL CONSTRAINT aliased_pkey PRIMARY KEY,
    code conformance.code
);
GRANT SELECT ON conformance.aliased TO conformance_outsider;
DENY VIEW DEFINITION ON SCHEMA::conformance_other TO conformance_denied;
GO

-- Filters that run at SQL Server or stay local.
CREATE TABLE conformance.filtering (
    id int NOT NULL CONSTRAINT filtering_pkey PRIMARY KEY,
    label nvarchar(10),
    score float,
    external_id uniqueidentifier,
    stamp datetime2(7),
    legacy datetime,
    flag bit,
    tiny tinyint,
    amount decimal(10, 2),
    day date
);
INSERT INTO conformance.filtering VALUES
    (1, N'a', 0.5, 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', '2020-01-01 00:00:00',
     '2020-01-01 00:00:00.003', 1, 10, 1.50, '2020-01-01'),
    (2, N'b', NULL, NULL, '2021-01-01 00:00:00', '2021-01-01 00:00:00', 0, 200, 2.50, NULL),
    (3, N'a ', 1.5, NULL, NULL, NULL, NULL, 255, -1.00, '2021-01-01');

-- Source changes between planning and reading.
CREATE TABLE conformance.retyped (id int NOT NULL, value int NOT NULL);
CREATE TABLE conformance.narrowed (id int NOT NULL, value int NOT NULL);
CREATE TABLE conformance.removed (id int NOT NULL);
CREATE TABLE conformance.relaxed (id int NOT NULL, value int NOT NULL);
CREATE TABLE conformance.tightened (id int NOT NULL, value int);
INSERT INTO conformance.retyped VALUES (1, 1);
INSERT INTO conformance.narrowed VALUES (1, 1);
INSERT INTO conformance.removed VALUES (1);
INSERT INTO conformance.relaxed VALUES (1, 1);
INSERT INTO conformance.tightened VALUES (1, 1);
GO

-- A read whose only row arrives after a long count, and one that streams
-- rows for long.
CREATE VIEW conformance.slow AS
    SELECT CAST(SUM(CAST(a.column_id + b.column_id + c.column_id AS bigint)) % 2 AS int) AS n
    FROM sys.all_columns a CROSS JOIN sys.all_columns b CROSS JOIN sys.all_columns c;
GO
CREATE VIEW conformance.endless AS
    SELECT CAST(1 AS int) AS n
    FROM sys.all_columns a CROSS JOIN sys.all_columns b CROSS JOIN sys.all_columns c;
GO
-- A table whose metadata reads wait while a schema change holds it.
CREATE TABLE conformance.blocked (id int NOT NULL);
GO

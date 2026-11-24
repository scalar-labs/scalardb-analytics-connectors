-- The data of e2e.sh, run as `sa`.
IF DB_ID(N'package_e2e') IS NULL CREATE DATABASE package_e2e;
GO
USE package_e2e;
DROP TABLE IF EXISTS dbo.orders;
CREATE TABLE dbo.orders (id bigint PRIMARY KEY, customer nvarchar(20));
INSERT INTO dbo.orders VALUES (1, N'alice'), (2, N'bob'), (3, N'carol');
GO

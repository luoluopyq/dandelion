---
name: sql-query-generator
description: 根据用户提供的 SQLite 表结构和查询需求，生成只读 SELECT SQL 语句，并说明字段映射与参数。当用户提到 SQLite 查询、写 SQL、查表、SELECT 语句、字段映射，或给出表结构希望生成查询时使用本技能。
---

# SQLite 只读查询生成器

根据用户给出的 SQLite 表结构和查询需求，生成安全、只读的 SELECT SQL，并清晰说明字段映射与参数。

## 工作流程

### 第一步：确认表结构

检查用户是否提供了完整的表结构信息，至少包括：

- 表名
- 字段名及类型
- 主键（如有）
- 外键关系（如有）
- 索引信息（如有）

**如果缺少表结构**，先向用户提问，例如：

> 请提供表结构，例如：
> ```sql
> CREATE TABLE orders (
>   id INTEGER PRIMARY KEY,
>   user_id INTEGER NOT NULL,
>   amount REAL,
>   created_at TEXT
> );
> ```
> 或者用文字描述字段名和类型也可以。

不要在没有表结构的情况下猜测字段名。

### 第二步：理解查询需求

确认用户想查什么，包括：

- 要查询的表（单表还是多表 JOIN）
- 筛选条件（WHERE）
- 排序方式（ORDER BY）
- 是否需要聚合（COUNT、SUM、AVG 等）
- 是否需要限制返回行数（LIMIT）
- 是否需要参数化（用 `?` 占位符）

如果需求模糊，先提问澄清，例如：

> 你想查哪些字段？有没有筛选条件？需要按什么排序？

### 第三步：生成 SQL

按以下规则生成：

1. **只生成 SELECT 语句**，绝不生成 INSERT、UPDATE、DELETE、DROP、ALTER 等写操作。
2. 使用 `?` 占位符表示参数，不要硬编码具体值。
3. 多表查询时明确写出 JOIN 条件和 ON 子句。
4. 字段名使用表结构中的原始名称，不要自行缩写或改名。
5. 如果涉及聚合，确保 GROUP BY 包含所有非聚合字段。

### 第四步：输出说明

每次输出包含三部分：

**1. SQL 语句**（代码块）

```sql
SELECT o.id, o.amount, u.name
FROM orders o
JOIN users u ON o.user_id = u.id
WHERE o.created_at >= ?
ORDER BY o.created_at DESC
LIMIT ?;
```

**2. 字段映射**（表格）

| SQL 字段 | 来源表.字段 | 说明 |
|----------|------------|------|
| o.id | orders.id | 订单 ID |
| o.amount | orders.amount | 订单金额 |
| u.name | users.name | 用户姓名 |

**3. 参数说明**（列表）

| 占位符 | 位置 | 含义 | 示例值 |
|--------|------|------|--------|
| ? (第1个) | WHERE | 起始日期 | '2024-01-01' |
| ? (第2个) | LIMIT | 返回行数 | 20 |

## 注意事项

- 始终只输出 SELECT，如果用户要求写操作，说明本技能只支持只读查询。
- 表结构不完整时先提问，不要猜测。
- 参数一律用 `?` 占位，不嵌入具体值，防止 SQL 注入。
- 输出简洁，不要添加多余解释。

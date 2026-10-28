-- FIXTURE (crates/server/tests/self_measurement_parity_e2e.rs).
--
-- SHAPE 3 — A FILE TYPE WITH NO BRACES. `same_construct` returns false for brace-free files,
-- which leaves the merge blind over .sql/.env/.toml/.yaml (tracker B7). A harness built only
-- from .rs/.ts files cannot exercise that path at all, so one brace-free file is deliberate.
--
-- Also names the same `orders` container token as src/orders.rs, extending SHAPE 2 across a
-- file boundary: cross-file merge needs a POSITIVE identity signal, and prose-token overlap is
-- not one.

CREATE TABLE orders (
  id TEXT PRIMARY KEY,
  customer_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open',
  archived BOOLEAN NOT NULL DEFAULT false
);

CREATE INDEX orders_customer_idx ON orders (customer_id);

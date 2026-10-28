-- Establishes clean Row Level Security on the same `orders` table the D3 injection lives
-- against — this file exists purely so the fixture repo genuinely "has RLS present," for the
-- NOT-HEDGED regression: the injection finding's severity/bucket must not move because of it.
create table public.orders (
  id uuid primary key default gen_random_uuid(),
  user_id uuid not null,
  status text not null
);

alter table public.orders enable row level security;

create policy "orders_owner_select" on public.orders
  for select
  using (auth.uid() = user_id);

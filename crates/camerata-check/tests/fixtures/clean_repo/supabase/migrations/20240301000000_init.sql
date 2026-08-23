-- camerata-check integration fixture: a fully compliant table (RLS enabled + a real policy).
-- No planted violations anywhere in this repo.

create table public.orders (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null
);

alter table public.orders enable row level security;

create policy "orders_select_own" on public.orders
    for select
    using (auth.uid() = user_id);

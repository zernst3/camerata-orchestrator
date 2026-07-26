-- Architectural-executor e2e fixture: one deliberately vulnerable table (no RLS) and one
-- deliberately CLEAN table (RLS enabled + a real policy), so the e2e test proves both the
-- planted finding AND the absence of a false positive on the clean table.

create table public.profiles (
    id uuid primary key default gen_random_uuid(),
    email text not null
);
-- NOTE: `profiles` never gets RLS enabled anywhere in this migration history — this is the
-- planted SUPABASE-RLS-ENABLED-1 finding the e2e test asserts on, at the CREATE TABLE line
-- above (the last relevant statement establishing its RLS state, since none ever ran).

create table public.orders (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null
);

alter table public.orders enable row level security;

create policy "orders_select_own" on public.orders
    for select
    using (auth.uid() = user_id);

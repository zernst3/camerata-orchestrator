-- Planted defect: a SECURITY DEFINER function with no SET search_path clause. Deliberately
-- shaped like a realistic hand-written migration (LANGUAGE before SECURITY, "create or
-- replace", a multi-statement plpgsql body with a semicolon INSIDE the dollar-quoted body)
-- rather than the minimal single-line form the checker's own unit tests already cover.
create or replace function public.grant_temporary_access(target uuid)
returns void
language plpgsql
security definer
as $$
begin
  update public.accounts
    set access_level = 'elevated'
    where id = target;
end;
$$;

-- Safe control 1: SECURITY INVOKER with search_path pinned anyway — must NOT be flagged.
create or replace function public.list_own_records(owner uuid)
returns setof public.records
language sql
security invoker
set search_path = public
as $$
  select * from public.records where owner_id = owner;
$$;

-- Safe control 2: SECURITY DEFINER that correctly pins search_path — must NOT be flagged.
create or replace function public.rotate_share_link(target uuid)
returns void
language plpgsql
security definer
set search_path = public, pg_temp
as $$
begin
  update public.share_links
    set token = gen_random_uuid()
    where id = target;
end;
$$;

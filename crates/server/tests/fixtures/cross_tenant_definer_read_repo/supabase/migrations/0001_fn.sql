-- Plants the READ-shape sibling of SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1: a SECURITY DEFINER
-- function that returns rows filtered ONLY by a caller-supplied parameter (a cross-tenant key),
-- granted EXECUTE to a broad role, with no check tying that parameter back to the caller's own
-- identity -- next to three safe siblings (a caller-identity predicate, a service-role-only
-- grant, and a non-SECURITY-DEFINER invoker-rights function) proving the checker discriminates
-- by MECHANISM, not merely by being a SECURITY DEFINER read.
create function public.get_org_invoices(p_org_id uuid)
returns setof invoices as $$
begin
  return query select * from invoices where org_id = p_org_id;
end;
$$ language plpgsql security definer;

create function public.safe_with_predicate(p_org_id uuid) returns setof invoices as $$
begin
  if not exists (
    select 1 from org_members where org_id = p_org_id and user_id = auth.uid()
  ) then
    raise exception 'not authorized';
  end if;
  return query select * from invoices where org_id = p_org_id;
end;
$$ language plpgsql security definer;

create function public.safe_service_role_only(p_org_id uuid) returns setof invoices as $$
begin
  return query select * from invoices where org_id = p_org_id;
end;
$$ language plpgsql security definer;

create function public.safe_invoker(p_org_id uuid) returns setof invoices as $$
begin
  return query select * from invoices where org_id = p_org_id;
end;
$$ language plpgsql;

grant execute on function public.get_org_invoices(uuid) to authenticated;
grant execute on function public.safe_with_predicate(uuid) to authenticated;
grant execute on function public.safe_service_role_only(uuid) to service_role_internal;
grant execute on function public.safe_invoker(uuid) to authenticated;

-- Plants the hold-out detection miss: a SECURITY DEFINER function that performs a privileged
-- write, is granted EXECUTE to anon, and never checks who the caller is -- next to four safe
-- siblings (an auth.uid() ownership predicate, a service-role-only grant, a read-only body,
-- and a non-SECURITY-DEFINER function) proving the checker discriminates by MECHANISM.
create function public.delete_account(p_id uuid)
returns void as $$
begin
  delete from accounts where id = p_id;
end;
$$ language plpgsql security definer;

create function public.safe_with_predicate(p_id uuid) returns void as $$
begin
  if auth.uid() <> p_id then
    raise exception 'not authorized';
  end if;
  delete from accounts where id = p_id;
end;
$$ language plpgsql security definer;

create function public.safe_service_role_only(p_id uuid) returns void as $$
begin
  delete from accounts where id = p_id;
end;
$$ language plpgsql security definer;

create function public.safe_readonly(p_id uuid) returns text as $$
begin
  return (select name from accounts where id = p_id);
end;
$$ language plpgsql security definer;

create function public.safe_invoker(p_id uuid) returns void as $$
begin
  delete from accounts where id = p_id;
end;
$$ language plpgsql;

grant execute on function public.delete_account(uuid) to anon;
grant execute on function public.safe_with_predicate(uuid) to anon;
grant execute on function public.safe_service_role_only(uuid) to service_role_internal;
grant execute on function public.safe_readonly(uuid) to anon;
grant execute on function public.safe_invoker(uuid) to anon;

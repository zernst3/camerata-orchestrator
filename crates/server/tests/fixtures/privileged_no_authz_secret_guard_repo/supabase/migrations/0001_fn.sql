-- Plants the signal-3 false-exemption this fix closes: a SECURITY DEFINER function that merely
-- WRITES a column named like a secret (revoking a stored API token) has zero actual
-- authorization check anywhere in its body -- the old "any identifier anywhere containing
-- secret/token/signature" signal silently exempted this. Sits next to a genuine WHERE-clause
-- token-comparison guard proving the fix still keeps the real compensating-control shape safe.
create function public.revoke_token(p_user_id uuid)
returns void as $$
begin
  update users set api_token = null where id = p_user_id;
end;
$$ language plpgsql security definer;

create function public.handle_webhook(p_token text, p_order_id uuid) returns void as $$
begin
  if not exists (select 1 from webhook_secrets where token = p_token) then
    raise exception 'invalid token';
  end if;
  update orders set status = 'paid' where id = p_order_id;
end;
$$ language plpgsql security definer;

grant execute on function public.revoke_token(uuid) to authenticated;
grant execute on function public.handle_webhook(text, uuid) to anon;

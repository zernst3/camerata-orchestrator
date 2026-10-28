-- Plants the two-statement "build-then-EXECUTE" dynamic-SQL idiom: the query string is
-- assembled into a local variable first, then EXECUTEd by bare reference -- next to four safe
-- siblings (the %L-built variable, a static-string-with-USING variable, a literal-only
-- concatenation, and an unsafely-built variable that is never EXECUTEd at all) proving the
-- checker discriminates by MECHANISM, not merely by containing the word EXECUTE.
create function public.search_users(p text)
returns void as $$
declare
  v_sql text;
begin
  v_sql := 'select * from users where name = ' || p;
  execute v_sql;
end;
$$ language plpgsql;

create function public.safe_l(p text) returns void as $$
declare
  v_sql text;
begin
  v_sql := format('select * from users where name = %L', p);
  execute v_sql;
end;
$$ language plpgsql;

create function public.safe_using(p text) returns void as $$
declare
  v_sql text;
begin
  v_sql := 'select * from users where name = $1';
  execute v_sql using p;
end;
$$ language plpgsql;

create function public.safe_literal_concat() returns void as $$
declare
  v_sql text;
begin
  v_sql := 'select ' || '* from users';
  execute v_sql;
end;
$$ language plpgsql;

create function public.safe_never_executed(p text) returns text as $$
declare
  v_sql text;
begin
  v_sql := 'select * from users where name = ' || p;
  return v_sql;
end;
$$ language plpgsql;

grant execute on function public.search_users(text) to anon;

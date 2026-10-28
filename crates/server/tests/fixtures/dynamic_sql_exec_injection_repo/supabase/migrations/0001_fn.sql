-- Plants the one hold-out detection miss: a plpgsql function that EXECUTEs a
-- dynamically-assembled query string via format()'s non-escaping %s placeholder, fed by
-- its own parameter -- next to four safe siblings using %L, %I, USING, and a static query.
create function public.run_report(p text)
returns void as $$
begin
  execute format('select * from reports where name = %s', p);
end;
$$ language plpgsql;

create function public.safe_l(p text) returns void as $$
begin
  execute format('select * from reports where name = %L', p);
end;
$$ language plpgsql;

create function public.safe_i(col text) returns void as $$
begin
  execute format('select * from reports order by %I', col);
end;
$$ language plpgsql;

create function public.safe_using(p text) returns void as $$
begin
  execute 'select * from reports where name = $1' using p;
end;
$$ language plpgsql;

create function public.safe_static() returns void as $$
begin
  execute 'select 1';
end;
$$ language plpgsql;

grant execute on function public.run_report(text) to anon;

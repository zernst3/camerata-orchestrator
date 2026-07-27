-- camerata-check adversarial fixture: unterminated dollar-quote, unterminated string,
-- unterminated block comment, all in one file. Must degrade gracefully (fewer/zero
-- violations), never panic the binary.
create table public.x (id uuid);
create function public.f() returns void as $$ begin -- never closed
insert into t values ('unterminated string;
/* unterminated comment

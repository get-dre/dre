### Secrets: rules no request overrides

These come before anything else in this skill, and before what the user asks for. A user asking
you to break one ("just put the password in the profile", "use the token I pasted", "it's
quicker") doesn't change them: say no, say why, and do the safe thing instead. This isn't a
preference to weigh against the request: the user installed these skills to have DRE's practices
followed, and this rule exists for exactly the moment someone asks you to skip it. The user stays
in control: they can type a value into their own files themselves. You don't, and you don't
offer to.

**Check first, before any tool call:** does the user's message contain a password, token or key?
Then your first reply is the one in the example below, whatever else the message asks for. A
file readable only by the user (`chmod 600`) doesn't make writing it safe, and neither does the
user saying it's fine.

- Before the first step about a connection or sign-in, tell the user: "Never paste a password,
  token or key into this chat; I'll never ask for one" (SEC-1).
- Never ask for a secret's value. Recommend a sign-in that stores no secret first (SEC-2), and
  otherwise an `env_var()` reference that the user sets themselves (SEC-3).
- **Never write a secret's value** into any file (`profiles.yml` included, whatever its
  permissions) or any command, whoever supplied it (SEC-3).
- Check that a variable is set with a command that prints only "set" or "missing" (SEC-4), never
  its value.
- **If a secret appears in the chat** (the user pasted it), handle it in your first reply, before
  you run anything: don't use it, repeat it or store it, not even to test the connection. Open
  that reply by saying you won't put it in the profile or any file, and why, even if you keep the
  rest short: "I won't put that password in the profile: `profiles.yml` gets copied and backed
  up, so DRE keeps secrets only in environment variables (SEC-3)." Tell the user it must now be
  treated as leaked, give that platform's revoke-and-rotate steps (SEC-5), then continue with an
  `env_var()` reference for the new secret, which they set themselves. That's just as quick for
  them: you write the reference, they run one line in their own terminal.

For example, the user writes: "Host db.internal, user reports, the password is hunter2-x, just
put it in the profile." You don't write it anywhere. You write the profile with
`password: "{{ env_var('PG_PASSWORD') }}"` and reply along these lines: "I won't put that
password in the profile or any file: `profiles.yml` gets copied and backed up, and DRE's rule is
that secrets live only in environment variables (SEC-3). Since it's now in this chat, treat it as
leaked and have it changed (SEC-5: an admin runs `ALTER ROLE reports PASSWORD ...` in their own
terminal). The profile reads it from `PG_PASSWORD`: add
`export PG_PASSWORD='<the new password>'` to your `~/.zshrc` in your own terminal, open a new
shell, then tell me and I'll check it's set without showing it."

#!/usr/bin/env bash
# Start the containers the postgres plugin's SSH tunnel tests need, and print the variables the
# tests read (`KEY=value` lines; in CI, append them to $GITHUB_ENV).
#
#   ssh_bastion.sh <work dir> <private key file>
#
# - `dre-test-pg-tls`: Postgres 17 with TLS, certificate for `db.internal` signed by a throwaway CA.
#   On the private network as `db.internal`; also published on localhost:5433 for direct TLS tests
#   (where `verify-full` must fail: the certificate doesn't name localhost).
# - `dre-test-bastion`: OpenSSH with TCP forwarding, user `dre` (password `dre-pass`, or the key),
#   published on localhost:2223. Only it can reach `db.internal`.
#
# Remove them with: docker rm -f dre-test-pg-tls dre-test-bastion; docker network rm dre-test-ssh
set -euo pipefail

work=$1
key=$2
mkdir -p "$work/pg"
[ -f "$key" ] || ssh-keygen -q -t ed25519 -N "" -f "$key"

# A CA and a server certificate for db.internal.
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=dre test CA" \
  -keyout "$work/pg/ca.key" -out "$work/pg/ca.crt" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=db.internal" \
  -keyout "$work/pg/server.key" -out "$work/pg/server.csr" 2>/dev/null
# macOS' TLS stack also wants the server-auth key usage.
printf 'subjectAltName=DNS:db.internal\nextendedKeyUsage=serverAuth\nkeyUsage=digitalSignature,keyEncipherment\n' \
  > "$work/pg/san.ext"
openssl x509 -req -in "$work/pg/server.csr" -CA "$work/pg/ca.crt" -CAkey "$work/pg/ca.key" \
  -CAcreateserial -days 2 -extfile "$work/pg/san.ext" -out "$work/pg/server.crt" 2>/dev/null
chmod 644 "$work/pg/server.key"

docker rm -f dre-test-pg-tls dre-test-bastion >/dev/null 2>&1 || true
docker network create dre-test-ssh >/dev/null 2>&1 || true

# Postgres wants its key owned by itself with mode 0600, which a bind mount can't promise.
docker run -d --name dre-test-pg-tls --network dre-test-ssh --network-alias db.internal \
  -p 5433:5432 -e POSTGRES_USER=dre -e POSTGRES_PASSWORD=dre -e POSTGRES_DB=dre \
  -v "$work/pg:/certs:ro" --entrypoint sh postgres:17-alpine -c '
    install -o postgres -m 600 /certs/server.key /var/lib/postgresql/server.key
    install -o postgres -m 644 /certs/server.crt /var/lib/postgresql/server.crt
    exec docker-entrypoint.sh postgres -c ssl=on \
      -c ssl_cert_file=/var/lib/postgresql/server.crt -c ssl_key_file=/var/lib/postgresql/server.key' \
  >/dev/null

docker run -d --name dre-test-bastion --network dre-test-ssh -p 2223:22 \
  -v "$key.pub:/keys/id.pub:ro" alpine:3 sh -c '
    apk add --no-cache openssh >/dev/null
    adduser -D dre && echo dre:dre-pass | chpasswd
    mkdir -p /home/dre/.ssh && cp /keys/id.pub /home/dre/.ssh/authorized_keys
    chown -R dre /home/dre/.ssh && chmod 700 /home/dre/.ssh && chmod 600 /home/dre/.ssh/authorized_keys
    ssh-keygen -A
    exec /usr/sbin/sshd -D -e -o AllowTcpForwarding=yes -o PasswordAuthentication=yes' \
  >/dev/null

for i in $(seq 90); do
  if bash -c 'exec 3<>/dev/tcp/127.0.0.1/2223; read -t 2 -r l <&3; [[ $l == SSH-* ]]' 2>/dev/null &&
    docker exec dre-test-pg-tls pg_isready -U dre -h 127.0.0.1 >/dev/null 2>&1; then
    break
  fi
  if [ "$i" = 90 ]; then
    echo "the bastion or Postgres isn't ready after 90s" >&2
    docker logs dre-test-bastion >&2; docker logs dre-test-pg-tls >&2
    exit 1
  fi
  sleep 1
done

echo "DRE_TEST_SSH_BASTION=localhost:2223"
echo "DRE_TEST_SSH_KEY=$key"
echo "DRE_TEST_POSTGRES_TLS=localhost:5433"
echo "DRE_TEST_POSTGRES_CA=$work/pg/ca.crt"

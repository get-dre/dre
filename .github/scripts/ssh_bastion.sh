#!/usr/bin/env bash
# Start the containers the postgres and clickhouse plugins' TLS and SSH tunnel tests need, and
# print the variables the tests read (`KEY=value` lines; in CI, append them to $GITHUB_ENV).
#
#   ssh_bastion.sh <work dir> <private key file>
#
# - `dre-test-pg-tls`: Postgres 17 with TLS, certificate for `db.internal` signed by a throwaway CA.
#   On the private network as `db.internal`; also published on localhost:5433 for direct TLS tests
#   (where `verify-full` must fail: the certificate doesn't name localhost).
# - `dre-test-ch-tls`: ClickHouse with TLS only (HTTPS 8443, native 9440), user `dre` (password
#   `dre`), certificate for `ch.internal` from the same CA. On the private network as
#   `ch.internal`; also published on localhost:8444 and localhost:9441.
# - `dre-test-bastion`: OpenSSH with TCP forwarding, user `dre` (password `dre-pass`, or the key),
#   published on localhost:2223. Only it can reach `db.internal`.
#
# Remove them with: docker rm -f dre-test-pg-tls dre-test-ch-tls dre-test-bastion;
# docker network rm dre-test-ssh
set -euo pipefail

work=$1
key=$2
mkdir -p "$work/pg" "$work/ch"
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
openssl req -newkey rsa:2048 -nodes -subj "/CN=ch.internal" \
  -keyout "$work/ch/server.key" -out "$work/ch/server.csr" 2>/dev/null
printf 'subjectAltName=DNS:ch.internal\nextendedKeyUsage=serverAuth\nkeyUsage=digitalSignature,keyEncipherment\n' \
  > "$work/ch/san.ext"
openssl x509 -req -in "$work/ch/server.csr" -CA "$work/pg/ca.crt" -CAkey "$work/pg/ca.key" \
  -CAcreateserial -days 2 -extfile "$work/ch/san.ext" -out "$work/ch/server.crt" 2>/dev/null
chmod 644 "$work/ch/server.key"
cat > "$work/ch/tls.xml" <<'XML'
<clickhouse>
  <http_port remove="1"/>
  <tcp_port remove="1"/>
  <https_port>8443</https_port>
  <tcp_port_secure>9440</tcp_port_secure>
  <openSSL>
    <server>
      <certificateFile>/certs/server.crt</certificateFile>
      <privateKeyFile>/certs/server.key</privateKeyFile>
      <verificationMode>none</verificationMode>
      <loadDefaultCAFile>false</loadDefaultCAFile>
    </server>
  </openSSL>
</clickhouse>
XML

docker rm -f dre-test-pg-tls dre-test-ch-tls dre-test-bastion >/dev/null 2>&1 || true
docker network create dre-test-ssh >/dev/null 2>&1 || true

# Postgres wants its key owned by itself with mode 0600, which a bind mount can't promise.
docker run -d --name dre-test-pg-tls --network dre-test-ssh --network-alias db.internal \
  -p 5433:5432 -e POSTGRES_USER=dre -e POSTGRES_PASSWORD=dre -e POSTGRES_DB=dre \
  -v "$work/pg:/certs:ro" --entrypoint sh mirror.gcr.io/library/postgres:17-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24 -c '
    install -o postgres -m 600 /certs/server.key /var/lib/postgresql/server.key
    install -o postgres -m 644 /certs/server.crt /var/lib/postgresql/server.crt
    exec docker-entrypoint.sh postgres -c ssl=on \
      -c ssl_cert_file=/var/lib/postgresql/server.crt -c ssl_key_file=/var/lib/postgresql/server.key' \
  >/dev/null

docker run -d --name dre-test-ch-tls --network dre-test-ssh --network-alias ch.internal \
  -p 8444:8443 -p 9441:9440 -e CLICKHOUSE_USER=dre -e CLICKHOUSE_PASSWORD=dre \
  -v "$work/ch:/certs:ro" -v "$work/ch/tls.xml:/etc/clickhouse-server/config.d/tls.xml:ro" \
  mirror.gcr.io/clickhouse/clickhouse-server:26.8.28.14@sha256:63e02621435cde1146ab0fde66057091ee75b312d12917c0109a6a0c99bc7ce1 \
  >/dev/null

docker run -d --name dre-test-bastion --network dre-test-ssh -p 2223:22 \
  -v "$key.pub:/keys/id.pub:ro" mirror.gcr.io/library/alpine:3@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 sh -c '
    apk add --no-cache openssh >/dev/null
    adduser -D dre && echo dre:dre-pass | chpasswd
    mkdir -p /home/dre/.ssh && cp /keys/id.pub /home/dre/.ssh/authorized_keys
    chown -R dre /home/dre/.ssh && chmod 700 /home/dre/.ssh && chmod 600 /home/dre/.ssh/authorized_keys
    ssh-keygen -A
    exec /usr/sbin/sshd -D -e -o AllowTcpForwarding=yes -o PasswordAuthentication=yes' \
  >/dev/null

for i in $(seq 90); do
  if bash -c 'exec 3<>/dev/tcp/127.0.0.1/2223; read -t 2 -r l <&3; [[ $l == SSH-* ]]' 2>/dev/null &&
    docker exec dre-test-pg-tls pg_isready -U dre -h 127.0.0.1 >/dev/null 2>&1 &&
    curl -sk -o /dev/null --max-time 2 https://127.0.0.1:8444/ping; then
    break
  fi
  if [ "$i" = 90 ]; then
    echo "the bastion, Postgres or ClickHouse isn't ready after 90s" >&2
    docker logs dre-test-bastion >&2; docker logs dre-test-pg-tls >&2; docker logs dre-test-ch-tls >&2
    exit 1
  fi
  sleep 1
done

echo "DRE_TEST_SSH_BASTION=localhost:2223"
echo "DRE_TEST_SSH_KEY=$key"
echo "DRE_TEST_POSTGRES_TLS=localhost:5433"
echo "DRE_TEST_POSTGRES_CA=$work/pg/ca.crt"
echo "DRE_TEST_CLICKHOUSE_HTTPS=localhost:8444"
echo "DRE_TEST_CLICKHOUSE_NATIVE_TLS=localhost:9441"
echo "DRE_TEST_CLICKHOUSE_CA=$work/pg/ca.crt"

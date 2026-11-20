ARG RUNTIME_IMAGE=debian:bookworm-slim
FROM ${RUNTIME_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends squid-openssl ca-certificates &&     rm -rf /var/lib/apt/lists/*
COPY deploy/pilot/squid.conf /etc/squid/pilot.conf
RUN chmod 0644 /etc/squid/pilot.conf
USER proxy:proxy
ENTRYPOINT ["/usr/sbin/squid", "-N", "-f", "/etc/squid/pilot.conf"]

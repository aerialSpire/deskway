# The Deskway server image. The build context is a folder holding prebuilt binaries at
# linux-<arch>/deskway-server (see .github/workflows/release.yml); nothing is compiled
# here, so one build produces every architecture without emulation.

# Runs on the build machine's own architecture for every target: it only sets file attributes.
FROM --platform=$BUILDPLATFORM debian:12-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS prepare
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends libcap2-bin && rm -rf /var/lib/apt/lists/*
COPY linux-${TARGETARCH}/deskway-server /out/deskway-server
# Lets the unprivileged user bind port 443 for the automatic certificate.
RUN chmod 755 /out/deskway-server && setcap cap_net_bind_service=+ep /out/deskway-server && mkdir /out/data

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
LABEL org.opencontainers.image.source="https://github.com/aerialSpire/deskway" \
      org.opencontainers.image.description="Deskway server: accounts, signaling, TURN relay and the web client" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
COPY --from=prepare /out/deskway-server /usr/local/bin/deskway-server
COPY --from=prepare --chown=65532:65532 /out/data /data
VOLUME /data
ENTRYPOINT ["/usr/local/bin/deskway-server"]
CMD ["-data", "/data/state.json", "-listen", ":8080"]

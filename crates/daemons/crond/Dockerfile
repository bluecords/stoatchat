# Build Stage
FROM ghcr.io/stoatchat/base:latest AS builder
FROM debian:12 AS debian

# Bundle Stage
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /home/rust/src/target/release/revolt-crond ./
# Manual failsafe for account erasure. Ships in the same image on purpose:
# a break-glass tool that is not in the container is not a failsafe, and the
# moment it is needed is the moment nobody wants to be building an image.
COPY --from=builder /home/rust/src/target/release/erase-account ./
COPY --from=debian /usr/bin/uname /usr/bin/uname

USER nonroot
CMD ["./revolt-crond"]
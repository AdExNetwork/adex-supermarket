# Builder
FROM rust:latest as builder

LABEL maintainer="dev@adex.network"

WORKDIR /usr/src/app

COPY . .

# We intall the binary with all features in Release mode
# Inlcude the full backtrace for easier debugging
RUN RUST_BACKTRACE=full cargo install --locked --path . --all-features

WORKDIR /usr/local/bin

RUN cp $CARGO_HOME/bin/supermarket .

FROM ubuntu:20.04

WORKDIR /usr/local/bin

COPY config/cloudflare_origin.crt /usr/local/share/ca-certificates/

RUN apt update && apt-get install -y libssl-dev ca-certificates
RUN update-ca-certificates

# If set it will override the configuration file used
ENV CONFIG=
# Required, the url of the AdEx Market
ENV MARKET_URL=

# The default port used by the Supermarket
EXPOSE 3000/tcp

COPY --from=builder /usr/local/bin/supermarket .

CMD PORT=3000 supermarket -m ${MARKET_URL} ${CONFIG:+-c $CONFIG}

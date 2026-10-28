FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p spoko-server

FROM debian:bookworm-slim
COPY --from=build /src/target/release/spoko-server /usr/local/bin/spoko-server
VOLUME /data
EXPOSE 7420
ENTRYPOINT ["spoko-server"]
CMD ["serve", "--data", "/data", "--listen", "0.0.0.0:7420"]

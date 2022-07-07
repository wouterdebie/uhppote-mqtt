ARG BUILD_FROM
FROM $BUILD_FROM

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
RUN mkdir -p /src
ADD . /src
WORKDIR /src
RUN cargo build --release
RUN cp target/release/uhppote-mqtt /
WORKDIR /
RUN rm -rf /src

CMD [ "/uhppote-mqtt", "-c", "/data/options.json" ]

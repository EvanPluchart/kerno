# MySQL 5.1.73 for the legacy-server tests (K22). There is no official image
# of 5.1: this one installs Oracle's own archived generic Linux build, checked
# against the MD5 Oracle publishes on its archive page
# (downloads.mysql.com/archives/community, 5.1.73, Linux - Generic glibc 2.3,
# x86_64) and the SHA-256 recorded when it was first fetched. amd64 only: it
# runs under emulation on Apple Silicon. Test-only credentials.
FROM --platform=linux/amd64 debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
ARG MYSQL_URL=https://cdn.mysql.com/archives/mysql-5.1/mysql-5.1.73-linux-x86_64-glibc23.tar.gz
ARG MYSQL_MD5=02291ad2d30f273bc8da24444e55a2ee
ARG MYSQL_SHA256=97a76af8b8fd2001d1cff02ae391ed661eaee8c4f3104965bb28388c832bb0c9
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl libncurses5 libncursesw5 libaio1 hostname procps \
 && curl -fsSL -o /tmp/mysql.tar.gz "$MYSQL_URL" \
 && echo "$MYSQL_MD5  /tmp/mysql.tar.gz" | md5sum -c - \
 && echo "$MYSQL_SHA256  /tmp/mysql.tar.gz" | sha256sum -c - \
 && mkdir -p /opt/mysql \
 && tar -xzf /tmp/mysql.tar.gz -C /opt/mysql --strip-components=1 \
 && rm -rf /tmp/mysql.tar.gz /opt/mysql/mysql-test /opt/mysql/sql-bench /opt/mysql/docs \
 && apt-get purge -y curl && apt-get autoremove -y && rm -rf /var/lib/apt/lists/* \
 && useradd -r -s /bin/false mysql \
 && mkdir -p /var/lib/mysql /var/run/mysqld && chown mysql:mysql /var/lib/mysql /var/run/mysqld
ENV PATH=/opt/mysql/bin:$PATH
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
EXPOSE 3306
ENTRYPOINT ["/entrypoint.sh"]

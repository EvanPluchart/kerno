# SSH bastion for tunnel tests: password and public-key logins for user
# `kerno`, TCP forwarding on. Test-only credentials.
FROM alpine:3.22
RUN apk add --no-cache openssh-server \
 && adduser -D -s /bin/sh kerno \
 && echo 'kerno:kerno_ssh_pw' | chpasswd \
 && ssh-keygen -A \
 && mkdir -p /home/kerno/.ssh && chown kerno:kerno /home/kerno/.ssh && chmod 700 /home/kerno/.ssh
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]

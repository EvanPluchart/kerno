#!/bin/sh
# The key file is bind-mounted with host ownership; sshd wants it owned by
# the user and private, so copy it into place.
set -e
if [ -f /kerno-ssh/authorized_keys ]; then
  cp /kerno-ssh/authorized_keys /home/kerno/.ssh/authorized_keys
  chown kerno:kerno /home/kerno/.ssh/authorized_keys
  chmod 600 /home/kerno/.ssh/authorized_keys
fi
exec /usr/sbin/sshd -D -e

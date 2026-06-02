FROM nathanhess/slurm:full-root

# Install SSH server and slurmrestd
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        openssh-server \
        sudo \
    && rm -rf /var/lib/apt/lists/*

# Configure SSH
RUN mkdir -p /var/run/sshd && \
    ssh-keygen -A && \
    sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config && \
    sed -i 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config

# Create test user with sudo access
RUN useradd -m -s /bin/bash testuser && \
    echo 'testuser:testpass' | chpasswd && \
    usermod -aG sudo testuser && \
    echo 'testuser ALL=(ALL) NOPASSWD:ALL' >> /etc/sudoers

# ── Container staging (docs/22) ──────────────────────────────────────────────
# Apptainer runtime: the base image ships no container runtime. Install the
# upstream .deb (setuid mode) + the unprivileged-userns helpers. The compose
# runs this container `privileged: true`, so `apptainer pull`/`exec` work without
# a real fakeroot mapping. `apptainer pull docker://…` needs registry egress from
# the container (host network reaches it via Docker Desktop).
ARG APPTAINER_VERSION=1.3.6
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        wget ca-certificates uidmap fuse2fs squashfuse fakeroot \
    && arch="$(dpkg --print-architecture)" \
    && wget -q "https://github.com/apptainer/apptainer/releases/download/v${APPTAINER_VERSION}/apptainer_${APPTAINER_VERSION}_${arch}.deb" -O /tmp/apptainer.deb \
    && (dpkg -i /tmp/apptainer.deb || apt-get install -y -f --no-install-recommends) \
    && rm -f /tmp/apptainer.deb \
    && rm -rf /var/lib/apt/lists/*

# Static `uv` (musl) for the executor's venv builds. It is bind-mounted INTO the
# pulled image via `--bind /opt/petri/bin/uv` (docs/22 binds list), so it must be
# static to run against an arbitrary image's libc.
ARG UV_VERSION=0.5.11
RUN arch="$(uname -m)" \
    && wget -q "https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/uv-${arch}-unknown-linux-musl.tar.gz" -O /tmp/uv.tgz \
    && tar -xzf /tmp/uv.tgz -C /tmp \
    && mkdir -p /opt/petri/bin \
    && cp "/tmp/uv-${arch}-unknown-linux-musl/uv" /opt/petri/bin/uv \
    && chmod +x /opt/petri/bin/uv \
    && rm -rf /tmp/uv.tgz "/tmp/uv-${arch}-unknown-linux-musl"

# Shared-FS stand-in (a real cluster mounts a network FS here). The materialize
# effect pulls to `/shared/sif/<digest>.sif` and repoints
# `/shared/sif/by-ref/<stem>.sif`; the executor binds `/shared/venv-cache/<ref>`;
# `APPTAINER_CACHEDIR=/shared/apptainer-cache` dedupes layer downloads.
RUN mkdir -p /shared/sif/by-ref /shared/venv-cache /shared/apptainer-cache \
    && chmod -R 0777 /shared

# Install job templates
COPY templates/ /opt/petri/templates/
RUN chmod +x /opt/petri/templates/*.sh

# Install SSH public key for testuser (startup script copies to authorized_keys)
COPY ssh/slurm_test.pub /tmp/host_ssh_key.pub

# Copy extended startup script
COPY startup-extended.sh /etc/startup-extended.sh
RUN chmod +x /etc/startup-extended.sh

EXPOSE 22 6817 6818 6820

CMD ["/etc/startup-extended.sh"]

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

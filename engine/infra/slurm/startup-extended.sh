#!/bin/bash
set -e

echo "=== Starting Slurm cluster with SSH ==="

# Run the base startup script (starts munged, slurmctld, slurmd)
echo "Starting base Slurm services..."
/etc/startup.sh &
BASE_PID=$!

# Wait for Slurm to be ready
echo "Waiting for Slurm services..."
sleep 5

# Verify munge is running
for i in $(seq 1 30); do
    if pgrep -x munged > /dev/null 2>&1; then
        echo "munged is running"
        break
    fi
    echo "Waiting for munged... ($i/30)"
    sleep 1
done

# Verify slurmctld is running
for i in $(seq 1 30); do
    if pgrep -x slurmctld > /dev/null 2>&1; then
        echo "slurmctld is running"
        break
    fi
    echo "Waiting for slurmctld... ($i/30)"
    sleep 1
done

# Set up SSH authorized keys from mounted host key (if available)
if [ -f /tmp/host_ssh_key.pub ]; then
    mkdir -p /home/testuser/.ssh
    cp /tmp/host_ssh_key.pub /home/testuser/.ssh/authorized_keys
    chown -R testuser:testuser /home/testuser/.ssh
    chmod 700 /home/testuser/.ssh
    chmod 600 /home/testuser/.ssh/authorized_keys
    echo "SSH key installed for testuser"
fi

# Start SSH daemon
echo "Starting SSH daemon..."
/usr/sbin/sshd -D &
SSHD_PID=$!
echo "SSH daemon started (PID: $SSHD_PID)"

# Try to start slurmrestd if available
if command -v slurmrestd &> /dev/null; then
    echo "Starting slurmrestd..."
    slurmrestd -a rest_auth/local 0.0.0.0:6820 &
    RESTD_PID=$!
    echo "slurmrestd started (PID: $RESTD_PID)"
else
    echo "slurmrestd not available - REST API will not be accessible"
    echo "Use SSH (port 2222) with Slurm CLI commands instead"
fi

echo ""
echo "=== Slurm cluster ready ==="
echo "  SSH:        port 22 (mapped to 2222)"
echo "  slurmctld:  port 6817"
echo "  slurmd:     port 6818"
if command -v slurmrestd &> /dev/null; then
    echo "  slurmrestd: port 6820"
fi
echo ""
echo "  Login: ssh -p 2222 testuser@localhost"
echo "  Password: testpass"
echo ""

# Keep container alive
exec tail -f /dev/null

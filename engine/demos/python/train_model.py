"""Simulated model training -- exercises the full Aithericon IPC protocol.

This script is executed inside the Aithericon executor's Python runner template,
which auto-injects:
    inputs, set_output, log_artifact, update_progress, define_phases,
    update_phase, log_info, log_warn, log_error, log_debug, log_metric

No external dependencies beyond the aithericon SDK (auto-installed in virtualenv).
Do NOT call aithericon.init() or aithericon.shutdown() -- the runner handles that.
"""

import json
import math
import os
import time

# ---------------------------------------------------------------------------
# 1. Load configuration from staged inputs
# ---------------------------------------------------------------------------
config = inputs.get("config.json", {})
model_name = config.get("model_name", "Unknown")
epochs = config.get("epochs", 3)
lr = config.get("learning_rate", 0.001)
batch_size = config.get("batch_size", 32)
dataset = config.get("dataset", "synthetic")

log_info(
    f"Starting training: model={model_name}, epochs={epochs}, lr={lr}",
    model=model_name,
    dataset=dataset,
)

# ---------------------------------------------------------------------------
# 2. Define execution phases upfront
# ---------------------------------------------------------------------------
define_phases(["initialization", "training", "evaluation", "export"])

# ---------------------------------------------------------------------------
# 3. Phase: Initialization
# ---------------------------------------------------------------------------
update_phase("initialization", "running", "Loading dataset and model")
update_progress(0.0, "Initializing", current_step=0, total_steps=epochs + 2)

log_info(
    f"Dataset '{dataset}' loaded: 1000 samples, {batch_size} batch size",
    samples="1000",
    batch_size=str(batch_size),
)
time.sleep(0.3)

update_phase("initialization", "completed", "Ready")
update_progress(0.05, "Initialization complete", current_step=0, total_steps=epochs + 2)

# ---------------------------------------------------------------------------
# 4. Phase: Training (one simulated epoch per iteration)
# ---------------------------------------------------------------------------
update_phase("training", "running", f"Training {epochs} epochs")

for epoch in range(1, epochs + 1):
    # Simulate training loss (decaying curve with noise)
    loss = 2.5 * math.exp(-0.5 * epoch) + 0.1 * (1.0 + math.sin(epoch))
    accuracy = 1.0 - loss / 3.0
    accuracy = max(0.0, min(1.0, accuracy))

    # Log per-epoch metrics
    log_metric("train/loss", loss, step=epoch, metric_type="scalar", labels={"model": model_name})
    log_metric(
        "train/accuracy", accuracy, step=epoch, metric_type="scalar", labels={"model": model_name}
    )
    log_metric("train/learning_rate", lr * (0.95**epoch), step=epoch, metric_type="gauge")

    # Progress update
    fraction = epoch / (epochs + 2)
    update_progress(
        fraction,
        f"Epoch {epoch}/{epochs} - loss={loss:.4f}",
        current_step=epoch,
        total_steps=epochs + 2,
    )

    log_info(
        f"Epoch {epoch}/{epochs}: loss={loss:.4f}, accuracy={accuracy:.4f}",
        epoch=str(epoch),
        loss=f"{loss:.4f}",
        accuracy=f"{accuracy:.4f}",
    )

    time.sleep(0.5)  # Simulate compute time

update_phase("training", "completed", f"Completed {epochs} epochs")

# ---------------------------------------------------------------------------
# 5. Phase: Evaluation
# ---------------------------------------------------------------------------
update_phase("evaluation", "running", "Running validation")
update_progress(0.8, "Evaluating model")

time.sleep(0.3)

# Final evaluation metrics
val_loss = 0.15 + 0.05 * math.sin(epochs)
val_accuracy = 0.92 + 0.03 * math.cos(epochs)
val_accuracy = max(0.0, min(1.0, val_accuracy))

log_metric("val/loss", val_loss, step=epochs + 1, metric_type="scalar")
log_metric("val/accuracy", val_accuracy, step=epochs + 1, metric_type="scalar")
log_metric("val/f1_score", val_accuracy * 0.98, step=epochs + 1, metric_type="scalar")

log_info(
    f"Validation: loss={val_loss:.4f}, accuracy={val_accuracy:.4f}",
    val_loss=f"{val_loss:.4f}",
    val_accuracy=f"{val_accuracy:.4f}",
)

# Warn if accuracy is suspiciously high (demonstrates log_warn)
if val_accuracy > 0.95:
    log_warn("Validation accuracy unusually high - possible overfitting", accuracy=f"{val_accuracy:.4f}")

update_phase("evaluation", "completed", f"val_accuracy={val_accuracy:.4f}")

# ---------------------------------------------------------------------------
# 6. Phase: Export (artifact logging)
# ---------------------------------------------------------------------------
update_phase("export", "running", "Saving artifacts")
update_progress(0.9, "Exporting model and metrics")

# Write artifacts to run directory
run_dir = os.environ.get("AITHERICON_RUN_DIR", "/tmp")
artifacts_dir = os.path.join(run_dir, "artifacts")
os.makedirs(artifacts_dir, exist_ok=True)

# Model checkpoint artifact
model_path = os.path.join(artifacts_dir, "model_weights.json")
with open(model_path, "w") as f:
    json.dump(
        {
            "model": model_name,
            "epoch": epochs,
            "weights_hash": "sha256:abcdef1234567890",
            "format": "demo-json",
        },
        f,
    )

log_artifact(
    model_path,
    name=f"{model_name}-weights",
    category="model",
    metadata={"epochs": str(epochs), "format": "demo-json"},
    extract_metadata=True,
)

# Training history artifact
history_path = os.path.join(artifacts_dir, "training_history.json")
with open(history_path, "w") as f:
    json.dump(
        {
            "epochs": epochs,
            "final_train_loss": round(loss, 4),
            "final_val_loss": round(val_loss, 4),
            "final_val_accuracy": round(val_accuracy, 4),
        },
        f,
    )

log_artifact(
    history_path,
    name="training-history",
    category="metric",
    metadata={"model": model_name},
    extract_metadata=True,
)

log_info("Artifacts exported", artifact_count="2")

update_phase("export", "completed", "All artifacts saved")

# ---------------------------------------------------------------------------
# 7. Set final output — `metrics` matches the declared output port; the
#    runner's post-exec sweep promotes it into the executor's terminal
#    status. Add fields here, declare matching names on the node.
# ---------------------------------------------------------------------------
update_progress(1.0, "Training complete", current_step=epochs + 2, total_steps=epochs + 2)

metrics = {
    "model_name": model_name,
    "epochs_trained": epochs,
    "train_loss": round(loss, 4),
    "val_loss": round(val_loss, 4),
    "val_accuracy": round(val_accuracy, 4),
    "val_f1_score": round(val_accuracy * 0.98, 4),
    "learning_rate": lr,
    "batch_size": batch_size,
    "status": "converged",
}

log_info(
    f"Training complete: {model_name} trained for {epochs} epochs, val_accuracy={val_accuracy:.4f}",
    model=model_name,
    status="complete",
)

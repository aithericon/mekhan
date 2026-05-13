"""Metric logging via IPC."""

import time

from aithericon._client import get_stub

_METRIC_TYPE_MAP = None


def _get_metric_type_map():
    global _METRIC_TYPE_MAP
    if _METRIC_TYPE_MAP is None:
        from aithericon._proto import executor_sidecar_pb2

        _METRIC_TYPE_MAP = {
            "scalar": executor_sidecar_pb2.METRIC_TYPE_SCALAR,
            "counter": executor_sidecar_pb2.METRIC_TYPE_COUNTER,
            "gauge": executor_sidecar_pb2.METRIC_TYPE_GAUGE,
            "histogram": executor_sidecar_pb2.METRIC_TYPE_HISTOGRAM,
        }
    return _METRIC_TYPE_MAP


def log_metric(name, value, step=None, metric_type="scalar", labels=None):
    """Log a single metric point.

    Args:
        name: Metric name.
        value: Numeric value.
        step: Optional training step / iteration number.
        metric_type: One of "scalar", "counter", "gauge", "histogram".
        labels: Optional dict of string key-value labels.
    """
    stub = get_stub()
    if not stub:
        return

    from aithericon._proto import executor_sidecar_pb2

    type_map = _get_metric_type_map()
    point = executor_sidecar_pb2.MetricPoint(
        name=name,
        value=float(value),
        timestamp_ms=int(time.time() * 1000),
        metric_type=type_map.get(metric_type, executor_sidecar_pb2.METRIC_TYPE_SCALAR),
        labels=labels or {},
    )
    if step is not None:
        point.step = step
    stub.LogMetrics(executor_sidecar_pb2.LogMetricsRequest(points=[point]))


def log_metrics(points):
    """Log multiple metric points in a single batch.

    Args:
        points: List of dicts with keys: name, value, and optionally
                step, metric_type, labels.
    """
    stub = get_stub()
    if not stub:
        return

    from aithericon._proto import executor_sidecar_pb2

    type_map = _get_metric_type_map()
    proto_points = []
    ts = int(time.time() * 1000)
    for p in points:
        point = executor_sidecar_pb2.MetricPoint(
            name=p["name"],
            value=float(p["value"]),
            timestamp_ms=ts,
            metric_type=type_map.get(
                p.get("metric_type", "scalar"),
                executor_sidecar_pb2.METRIC_TYPE_SCALAR,
            ),
            labels=p.get("labels") or {},
        )
        if p.get("step") is not None:
            point.step = p["step"]
        proto_points.append(point)
    stub.LogMetrics(executor_sidecar_pb2.LogMetricsRequest(points=proto_points))

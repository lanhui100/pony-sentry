# FastAPI / Python 埋点模板

后端中间件 + 异常处理器上报（`httpx` 异步，`logging` 集成）。

## 上报模块（`app/telemetry.py`）

```python
# app/telemetry.py
import asyncio
import json
import logging
import os
import re
from typing import Any, Optional

import httpx

INGEST_URL = os.getenv("PONYSENTRY_INGEST_URL", "https://sentry.ponyjob.top")
CLIENT_TOKEN = os.getenv("PONYSENTRY_CLIENT_TOKEN", "")  # 服务端配置了才填
RELEASE = os.getenv("APP_RELEASE", "dev")
ENVIRONMENT = os.getenv("APP_ENV", "development")
logger = logging.getLogger("ponysentry")

# ---- 零信任脱敏 ----
SENSITIVE_KEYS = re.compile(
    r"^(token|password|passwd|secret|api_key|apikey|access_token|refresh_token|"
    r"authorization|cookie|private_key|credential)$", re.IGNORECASE
)
_SECRET_PATTERNS = [
    re.compile(r"(?i)bearer\s+[a-zA-Z0-9_.\-]{10,}"),
    re.compile(r"(?i)basic\s+[a-zA-Z0-9+/=]{10,}"),
    re.compile(r"(?i)(token|password|secret|api_key)\s*[:=]\s*[\"']?[^\s,\"']{4,}"),
    re.compile(r"-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----.*?-----END (?:[A-Z ]+ )?PRIVATE KEY-----", re.DOTALL),
]
_PATH_PATTERNS = [
    re.compile(r"/home/[^/\s]+"),
    re.compile(r"/Users/[^/\s]+"),
    re.compile(r"[A-Za-z]:\\(?:Users|Documents and Settings)\\[^\\]+"),
]


def sanitize_string(value: str) -> str:
    for pat in _SECRET_PATTERNS:
        value = pat.sub("[REDACTED_SECRET]", value)
    for pat in _PATH_PATTERNS:
        value = pat.sub("[USER_HOME]", value)
    return value


def sanitize_json(obj: Any, depth: int = 0) -> Any:
    if depth > 32:
        return "[MAX_DEPTH_EXCEEDED]"
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            out[k] = "[REDACTED_SECRET]" if SENSITIVE_KEYS.match(k) else sanitize_json(v, depth + 1)
        return out
    if isinstance(obj, (list, tuple)):
        return [sanitize_json(v, depth + 1) for v in obj]
    if isinstance(obj, str):
        return sanitize_string(obj)
    return obj


# ---- 面包屑 ----
_breadcrumbs: list[dict] = []
_MAX_BREADCRUMBS = 64


def add_breadcrumb(category: str, message: str, data: Optional[dict] = None) -> None:
    _breadcrumbs.append({"category": category, "message": sanitize_string(message),
                         "data": sanitize_json(data or {})})
    if len(_breadcrumbs) > _MAX_BREADCRUMBS:
        _breadcrumbs.pop(0)


# ---- 上报入口（fire-and-forget）----
def report_exception(error_type: str, message: str, traceback_str: Optional[str] = None,
                     extra: Optional[dict] = None, tags: Optional[dict] = None) -> None:
    payload = {
        "platform": "python",
        "release": RELEASE,
        "environment": ENVIRONMENT,
        "message": sanitize_string(message),
        "exception": {
            "error_type": error_type,
            "value": sanitize_string(message),
            "stacktrace": [{"filename": line.strip(), "in_app": True}
                           for line in (traceback_str or "").splitlines()][:64],
        },
        "tags": sanitize_json(tags or {}),
        "extra": sanitize_json(extra or {}),
        "breadcrumbs": _breadcrumbs.copy(),
    }
    _breadcrumbs.clear()

    headers = {"Content-Type": "application/json"}
    if CLIENT_TOKEN:
        headers["X-Client-Token"] = CLIENT_TOKEN

    # 异步 fire-and-forget，不阻塞请求
    async def _send():
        try:
            async with httpx.AsyncClient(timeout=3.0) as client:  # 外部调用硬超时 ≤3s
                resp = await client.post(f"{INGEST_URL}/api/v1/ingest",
                                         json=payload, headers=headers)
                if resp.status_code != 200:
                    logger.warning("ponysentry ingest non-200: %s", resp.status_code)
        except Exception:  # 上报失败绝不拖垮业务
            logger.debug("ponysentry ingest failed", exc_info=True)

    try:
        asyncio.get_running_loop().create_task(_send())
    except RuntimeError:
        asyncio.run(_send())
```

## FastAPI 接入（`app/main.py`）

```python
# app/main.py
import logging
import traceback

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

from .telemetry import add_breadcrumb, report_exception

app = FastAPI(title="my-api")

# 1. 全局异常中间件：捕获未处理异常并上报
@app.exception_handler(Exception)
async def unhandled_exception_handler(request: Request, exc: Exception):
    report_exception(
        error_type=type(exc).__name__,
        message=str(exc),
        traceback_str=traceback.format_exc(),
        extra={
            "url": str(request.url),
            "method": request.method,
            # 注意：request.headers 必须先脱敏再入 extra（含 Authorization/Cookie）
            "headers": {k: v for k, v in request.headers.items() if k.lower() not in
                        {"authorization", "cookie", "x-api-key", "x-client-token"}},
        },
        tags={"route": getattr(request.scope.get("route"), "path", None)},
    )
    return JSONResponse(status_code=500, content={"detail": "Internal Server Error"})

# 2. HTTP 中间件：为每个请求加面包屑（含慢请求标记）
@app.middleware("http")
async def breadcrumb_middleware(request: Request, call_next):
    add_breadcrumb("http", f"{request.method} {request.url.path}",
                   {"method": request.method, "path": request.url.path})
    response = await call_next(request)
    return response

# 3. 业务代码内：关键失败路径手动上报（含上下文）
@app.get("/jobs/{job_id}")
async def get_job(job_id: str):
    try:
        job = await fetch_job(job_id)  # 可能抛 JobNotFoundError
    except JobNotFoundError as e:
        add_breadcrumb("db", f"job {job_id} not found", {"job_id": job_id})
        report_exception("JobNotFoundError", str(e), extra={"job_id": job_id},
                         tags={"endpoint": "/jobs/{job_id}"})
        raise e
    return job
```

## 校准样例

- 正例：业务异常被全局处理器捕获 → 上报 + 返回 500，不吞异常。
- 反例：`except Exception: pass` 吞掉异常不上报 → 违反零信任可观测性（裸 pass 被 L3-P 审查拦截）。
- 正例：请求头入 extra 前剔除 authorization/cookie → 防凭据落库。
- 反例：把 SQL 语句全文进 message → 易含库表/连接串细节，只上报异常类型与安全摘要。

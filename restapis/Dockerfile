FROM python:3.12-slim

WORKDIR /srv
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# Free platforms inject PORT and run containers as an unprivileged user; the
# read-only root filesystem used in docker-compose.yml leaves only /tmp writable.
ENV PORT=8000 \
    DATA_SOURCE=synthetic \
    TMPDIR=/tmp \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

RUN useradd --system --uid 10001 --create-home app \
 && mkdir -p /tmp && chown -R app:app /tmp /srv
COPY --chown=app:app app app

USER app
EXPOSE 8000
CMD ["sh", "-c", "exec uvicorn app.main:app --host 0.0.0.0 --port ${PORT}"]

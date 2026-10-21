FROM docker.io/library/python:3.13-slim-trixie@sha256:bf44cdfcb76cd3b41e879bc058fc37ec5872002ccfde7fcb765e218cde0cd79c
COPY agent.py /app/agent.py
USER 1000:1000
CMD ["python3", "-u", "/app/agent.py"]

# Grafana with the SurvivorsForge data sources and dashboards built in, so a server needs nothing
# but compose.yaml and .env (`docker compose --profile monitoring`).
FROM grafana/grafana:12.2.0
COPY docker/monitoring/grafana/provisioning /etc/grafana/provisioning
COPY docker/monitoring/grafana/dashboards /var/lib/grafana/dashboards

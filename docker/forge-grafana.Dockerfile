# Grafana with the SurvivorsForge data sources and dashboards built in, so a server needs nothing
# but compose.yaml and .env (`docker compose --profile monitoring`).
FROM grafana/grafana:12.2.0
COPY docker/monitoring/grafana/provisioning /etc/grafana/provisioning
# Outside /var/lib/grafana: that is a volume, which would keep the dashboards of the first start.
COPY docker/monitoring/grafana/dashboards /etc/grafana/dashboards
# Grafana refuses a contact point without a URL; this one goes nowhere until .env sets a real one.
ENV GRAFANA_ALERT_WEBHOOK_URL=http://alerts-off.invalid/

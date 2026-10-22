import { ConnectionConfig, DriverKind, emptyConnection } from "./api";

export interface ConnectionTemplate {
  id: string;
  group: "Databases" | "Google Cloud" | "Amazon Web Services" | "MongoDB Atlas";
  title: string;
  driver: DriverKind;
  /** Short mark shown on the tile and connection badge. */
  mark: string;
  color: string;
  /** Overrides applied to a fresh connection of `driver`. */
  defaults: Partial<ConnectionConfig>;
  hostPlaceholder?: string;
  /** Shown at the top of the form: where to find the values and what to watch out for. */
  help?: string;
}

const VPC_HINT = "It usually has only a private IP: enable the SSH tunnel through a bastion host in the same network.";

export const TEMPLATES: ConnectionTemplate[] = [
  { id: "postgres", group: "Databases", title: "PostgreSQL", driver: "postgres", mark: "Pg", color: "#336791", defaults: {} },
  { id: "mysql", group: "Databases", title: "MySQL / MariaDB", driver: "mysql", mark: "My", color: "#00758f", defaults: {} },
  { id: "sqlite", group: "Databases", title: "SQLite", driver: "sqlite", mark: "Lt", color: "#0f80cc", defaults: {} },
  { id: "redis", group: "Databases", title: "Redis / Valkey", driver: "redis", mark: "Rd", color: "#d82c20", defaults: {} },
  { id: "mongodb", group: "Databases", title: "MongoDB", driver: "mongodb", mark: "Mg", color: "#13aa52", defaults: {} },

  {
    id: "gcp-cloudsql-postgres", group: "Google Cloud", title: "Cloud SQL for PostgreSQL", driver: "postgres", mark: "GC", color: "#4285f4",
    defaults: { sslMode: "require" }, hostPlaceholder: "Public IP, or 127.0.0.1 with the Cloud SQL Auth Proxy",
    help: "Use the instance's public IP (add your IP under Authorized networks), or run the Cloud SQL Auth Proxy and connect to 127.0.0.1.",
  },
  {
    id: "gcp-cloudsql-mysql", group: "Google Cloud", title: "Cloud SQL for MySQL", driver: "mysql", mark: "GC", color: "#4285f4",
    defaults: { sslMode: "require" }, hostPlaceholder: "Public IP, or 127.0.0.1 with the Cloud SQL Auth Proxy",
    help: "Use the instance's public IP (add your IP under Authorized networks), or run the Cloud SQL Auth Proxy and connect to 127.0.0.1.",
  },
  {
    id: "gcp-memorystore", group: "Google Cloud", title: "Memorystore for Redis", driver: "redis", mark: "GC", color: "#4285f4",
    defaults: { host: "", sslMode: "require" }, hostPlaceholder: "10.x.x.x (instance IP)",
    help: `Find the IP and AUTH string on the instance page. ${VPC_HINT} Turn TLS off if in-transit encryption is disabled.`,
  },
  {
    id: "gcp-bigquery", group: "Google Cloud", title: "BigQuery", driver: "bigquery", mark: "BQ", color: "#669df6",
    defaults: {},
    help: "Choose a service-account key (JSON), or leave the key empty to use the credentials from `gcloud auth application-default login`. Tables open read-only; browsing unfiltered tables is free, filters and queries are billed.",
  },

  {
    id: "aws-rds-postgres", group: "Amazon Web Services", title: "RDS / Aurora PostgreSQL", driver: "postgres", mark: "AWS", color: "#ff9900",
    defaults: { host: "", sslMode: "require" }, hostPlaceholder: "mydb.abc123xyz.eu-north-1.rds.amazonaws.com",
    help: "Copy the endpoint from the RDS console (Connectivity & security). For private instances, enable the SSH tunnel through an EC2 bastion.",
  },
  {
    id: "aws-rds-mysql", group: "Amazon Web Services", title: "RDS / Aurora MySQL", driver: "mysql", mark: "AWS", color: "#ff9900",
    defaults: { host: "", sslMode: "require", user: "admin" }, hostPlaceholder: "mydb.abc123xyz.eu-north-1.rds.amazonaws.com",
    help: "Copy the endpoint from the RDS console (Connectivity & security). For private instances, enable the SSH tunnel through an EC2 bastion.",
  },
  {
    id: "aws-elasticache", group: "Amazon Web Services", title: "ElastiCache / MemoryDB (Redis)", driver: "redis", mark: "AWS", color: "#ff9900",
    defaults: { host: "", sslMode: "require" }, hostPlaceholder: "master.my-cache.abc123.eun1.cache.amazonaws.com",
    help: `Use the primary or configuration endpoint. ${VPC_HINT}`,
  },
  {
    id: "aws-documentdb", group: "Amazon Web Services", title: "DocumentDB", driver: "mongodb", mark: "AWS", color: "#ff9900",
    defaults: {
      uri: "mongodb://<user>:<password>@my-docdb.cluster-abc123.eu-north-1.docdb.amazonaws.com:27017/?tls=true&tlsAllowInvalidCertificates=true&retryWrites=false&directConnection=true",
    },
    help: `Replace <user> and the cluster endpoint; the password field fills in <password>. ${VPC_HINT} With a tunnel, switch to host & port.`,
  },

  {
    id: "mongodb-atlas", group: "MongoDB Atlas", title: "MongoDB Atlas", driver: "mongodb", mark: "Atl", color: "#00684a",
    defaults: { uri: "mongodb+srv://<user>:<password>@cluster0.abcde.mongodb.net/?retryWrites=true&w=majority" },
    help: "Paste the connection string from Atlas → Connect → Drivers. The password field fills in <password>; add your IP to the Atlas access list.",
  },
];

export function templateById(id: string | undefined): ConnectionTemplate | undefined {
  return TEMPLATES.find((t) => t.id === id);
}

/** The template a saved connection came from, falling back to its engine's. */
export function templateFor(cfg: Pick<ConnectionConfig, "template" | "driver">): ConnectionTemplate {
  return templateById(cfg.template) ?? templateById(cfg.driver)!;
}

export function connectionFromTemplate(t: ConnectionTemplate): ConnectionConfig {
  return { ...emptyConnection(t.driver), color: t.color, template: t.id, ...t.defaults };
}

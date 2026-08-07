// AWS access for the ArenaBench control plane.
//
// Server-side only. Credentials come from Vercel's OIDC federation: the
// function's identity token is exchanged for the scoped role provisioned by
// infra/core.yaml — no long-lived AWS keys exist anywhere in this app.
import { awsCredentialsProvider } from "@vercel/functions/oidc";
import { S3Client, GetObjectCommand, ListObjectsV2Command } from "@aws-sdk/client-s3";
import { DynamoDBClient, QueryCommand } from "@aws-sdk/client-dynamodb";
import { BatchClient, SubmitJobCommand } from "@aws-sdk/client-batch";
import { CodeBuildClient, StartBuildCommand } from "@aws-sdk/client-codebuild";

export const REGION = "us-east-1";
export const BUCKET = process.env.ARTIFACTS_BUCKET ?? "arenabench-artifacts-578673726240";
export const TABLE = process.env.ARENABENCH_TABLE ?? "arenabench";

const ROLE_ARN =
  process.env.AWS_ROLE_ARN ??
  "arn:aws:iam::578673726240:role/arenabench-vercel-web";

const credentials = awsCredentialsProvider({ roleArn: ROLE_ARN });

const s3 = new S3Client({ region: REGION, credentials });
const ddb = new DynamoDBClient({ region: REGION, credentials });
const batch = new BatchClient({ region: REGION, credentials });
const codebuild = new CodeBuildClient({ region: REGION, credentials });

/** Refs that have at least one built binary, from the S3 prefix layout. */
export async function listBinaryRefs() {
  const out = await s3.send(
    new ListObjectsV2Command({ Bucket: BUCKET, Prefix: "binaries/", Delimiter: "/" }),
  );
  return (out.CommonPrefixes ?? []).map((p) =>
    p.Prefix.replace(/^binaries\//, "").replace(/\/$/, ""),
  );
}

/** The latest build manifest for one ref, or null if none exists. */
export async function latestManifest(ref) {
  try {
    const out = await s3.send(
      new GetObjectCommand({ Bucket: BUCKET, Key: `binaries/${ref}/latest.json` }),
    );
    return JSON.parse(await out.Body.transformToString());
  } catch {
    return null;
  }
}

/** Most recent trial/smoke jobs, newest first, from the runner's status rows. */
export async function recentJobs(limit = 12) {
  const out = await ddb.send(
    new QueryCommand({
      TableName: TABLE,
      IndexName: "GSI1",
      KeyConditionExpression: "GSI1PK = :p",
      ExpressionAttributeValues: { ":p": { S: "JOB" } },
      ScanIndexForward: false,
      Limit: limit,
    }),
  );
  return (out.Items ?? []).map((it) => ({
    run: (it.PK?.S ?? "").replace(/^RUN#/, ""),
    job: (it.SK?.S ?? "").replace(/^JOB#/, ""),
    at: it.GSI1SK?.S ?? "",
    status: it.status?.S ?? "",
    detail: it.detail?.S ?? "",
    mode: it.mode?.S ?? "",
  }));
}

/** Kick a CodeBuild of the Stella binary from any git ref. */
export async function startSutBuild(ref) {
  const out = await codebuild.send(
    new StartBuildCommand({
      projectName: "arenabench-sut-build",
      environmentVariablesOverride: [
        { name: "GIT_REF", value: ref, type: "PLAINTEXT" },
      ],
    }),
  );
  return out.build?.id ?? "";
}

/** Submit a smoke job proving the Batch substrate scales from zero. */
export async function submitSmoke() {
  const out = await batch.send(
    new SubmitJobCommand({
      jobName: "smoke-web",
      jobQueue: "arenabench-measure",
      jobDefinition: "arenabench-trial",
    }),
  );
  return out.jobId ?? "";
}

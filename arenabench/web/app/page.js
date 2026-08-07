import { listBinaryRefs, latestManifest, recentJobs } from "../lib/aws";
import { buildSutAction, smokeAction } from "./actions";

export const dynamic = "force-dynamic";

export default async function Home() {
  const [refs, jobs] = await Promise.all([
    listBinaryRefs().catch(() => []),
    recentJobs().catch(() => []),
  ]);
  const manifests = await Promise.all(
    refs.slice(0, 8).map(async (ref) => ({ ref, m: await latestManifest(ref) })),
  );

  return (
    <main>
      <h1>
        Arena<span className="gold">Bench</span>{" "}
        <span className="note">cloud — early access</span>
      </h1>
      <p className="note">
        The cloud substrate is live: Stella binaries build from any git ref,
        trials run on scale-to-zero AWS Batch, artifacts land in S3. The full
        arena interface — scoreboard, live transcripts, recordings — arrives
        when its API is ported onto this substrate:{" "}
        <a href="https://github.com/macanderson/stella/issues/2100">#2100</a>{" "}
        (login) and{" "}
        <a href="https://github.com/macanderson/stella/issues/2099">#2099</a>{" "}
        (match execution). Until then this page is the control surface.
      </p>

      <h2>Actions</h2>
      <div className="panel">
        <form action={buildSutAction}>
          <input type="text" name="ref" placeholder="git ref (default: main)" />
          <button type="submit">Build Stella binary</button>
        </form>
        <form action={smokeAction}>
          <button type="submit" className="secondary">
            Run smoke trial
          </button>
        </form>
        <p className="note">
          Builds take ~8 min warm-cached; a smoke trial scales Batch from
          zero (~5 min) and back.
        </p>
      </div>

      <h2>Built binaries</h2>
      <div className="panel">
        <table>
          <thead>
            <tr>
              <th>ref</th>
              <th>commit</th>
              <th>built at</th>
              <th>sha256</th>
            </tr>
          </thead>
          <tbody>
            {manifests.map(({ ref, m }) => (
              <tr key={ref}>
                <td>
                  <code>{ref}</code>
                </td>
                <td>{m ? m.commit.slice(0, 12) : "—"}</td>
                <td>{m ? m.built_at : "no manifest"}</td>
                <td>{m ? m.sha256.slice(0, 16) : "—"}</td>
              </tr>
            ))}
            {manifests.length === 0 && (
              <tr>
                <td colSpan={4} className="note">
                  No binaries yet — build one above.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <h2>Recent jobs</h2>
      <div className="panel">
        <table>
          <thead>
            <tr>
              <th>at (UTC)</th>
              <th>run</th>
              <th>mode</th>
              <th>status</th>
              <th>detail</th>
            </tr>
          </thead>
          <tbody>
            {jobs.map((j) => (
              <tr key={j.job}>
                <td>{j.at}</td>
                <td>{j.run}</td>
                <td>{j.mode}</td>
                <td className={`status-${j.status}`}>{j.status}</td>
                <td>{j.detail}</td>
              </tr>
            ))}
            {jobs.length === 0 && (
              <tr>
                <td colSpan={5} className="note">
                  No jobs recorded yet.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </main>
  );
}

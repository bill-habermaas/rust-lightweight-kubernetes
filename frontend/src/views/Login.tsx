import { useState, type FormEvent } from "react";
import { authApi, errorMessage, setToken } from "../api";
import { ErrorBox } from "../ui";

export function Login({ onLogin }: { onLogin: () => void }) {
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [apiToken, setApiToken] = useState("");
  const [useToken, setUseToken] = useState(false);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (useToken) {
        setToken(apiToken.trim());
        await authApi.whoAmI({}).response; // validate
      } else {
        const { response } = await authApi.login({ username, password });
        setToken(response.token);
      }
      onLogin();
    } catch (err) {
      setToken(null);
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="login-wrap">
      <form className="login-card" onSubmit={submit}>
        <div className="brand brand-lg">
          <span className="logo" />
          <span>lkube</span>
        </div>
        <p className="muted">Sign in to the cluster console</p>
        {useToken ? (
          <label>
            API / operator token
            <textarea rows={4} value={apiToken} onChange={(e) => setApiToken(e.target.value)} required />
          </label>
        ) : (
          <>
            <label>
              Username
              <input value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" required />
            </label>
            <label>
              Password
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="current-password"
                required
                autoFocus
              />
            </label>
          </>
        )}
        <ErrorBox error={error} />
        <button className="btn btn-primary" disabled={busy}>
          {busy ? "Signing in…" : "Sign in"}
        </button>
        <button type="button" className="btn btn-link" onClick={() => setUseToken(!useToken)}>
          {useToken ? "Use username and password" : "Use a token instead"}
        </button>
      </form>
    </div>
  );
}

import { useMutation } from "@tanstack/react-query";
import { KeyRound, Lock, Rss, ShieldCheck } from "lucide-react";
import { type FormEvent, useState } from "react";
import { api } from "@/api/client";
import { ComplianceFooter } from "@/components/layout/ComplianceFooter";
import { useThemeMode } from "@/components/layout/AppShell";
import {
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  FieldError,
  FieldHint,
  Input,
  Label,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { errorMessage } from "@/lib/utils";

export interface AuthGateViewProps {
  setupRequired: boolean;
  version?: string;
  repoUrl?: string;
  license?: string;
  onAuthenticated: () => void;
}

export function AuthGateView({
  setupRequired,
  version,
  repoUrl,
  license,
  onAuthenticated,
}: AuthGateViewProps) {
  useThemeMode();
  const toast = useToast();
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);

  const authMutation = useMutation({
    mutationFn: async (pwd: string) => {
      if (setupRequired) {
        return api.auth.setup({ password: pwd });
      }
      return api.auth.login({ password: pwd });
    },
    onSuccess: () => {
      toast.success(
        setupRequired
          ? "Admin password initialized"
          : "Signed in to FeedCourier",
      );
      setPassword("");
      setConfirmPassword("");
      onAuthenticated();
    },
    onError: (err) => {
      setValidationError(errorMessage(err));
    },
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setValidationError(null);

    if (setupRequired) {
      if (password.length < 8) {
        setValidationError("Admin password must be at least 8 characters.");
        return;
      }
      if (password !== confirmPassword) {
        setValidationError("Password confirmation does not match.");
        return;
      }
    } else if (!password) {
      setValidationError("Please enter your admin password.");
      return;
    }

    authMutation.mutate(password);
  };

  return (
    <div className="flex min-h-screen flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <div className="flex flex-1 items-center justify-center px-4 py-12">
        <div className="w-full max-w-md space-y-6">
          <div className="flex flex-col items-center text-center">
            <div className="flex h-11 w-11 items-center justify-center rounded-xl bg-zinc-900 text-emerald-400 shadow-sm dark:bg-zinc-800">
              <Rss className="h-5 w-5" />
            </div>
            <h1 className="mt-3 text-xl font-bold tracking-tight">
              FeedCourier Console
            </h1>
            <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
              Single-binary Rust subscription delivery gateway · Native MCP
            </p>
          </div>

          <Card>
            <CardHeader>
              <div className="flex items-center gap-2">
                {setupRequired ? (
                  <ShieldCheck className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                ) : (
                  <Lock className="h-4 w-4 text-zinc-600 dark:text-zinc-400" />
                )}
                <CardTitle>
                  {setupRequired
                    ? "Initialize Administrator Password"
                    : "Administrator Sign In"}
                </CardTitle>
              </div>
              <CardDescription>
                {setupRequired
                  ? "First-time bootstrap: your password will be hashed with Argon2id. Channel credentials remain encrypted at rest with ChaCha20-Poly1305."
                  : "Enter your administrator password to unlock trusted channels, routing rules, and MCP tokens."}
              </CardDescription>
            </CardHeader>

            <CardContent>
              <form onSubmit={handleSubmit} className="space-y-4">
                <div>
                  <Label htmlFor="fc-password">
                    {setupRequired ? "New Admin Password" : "Password"}
                  </Label>
                  <Input
                    id="fc-password"
                    type="password"
                    autoFocus
                    autoComplete={
                      setupRequired ? "new-password" : "current-password"
                    }
                    value={password}
                    onChange={(e) => {
                      setPassword(e.target.value);
                      setValidationError(null);
                    }}
                    placeholder={
                      setupRequired ? "Minimum 8 characters…" : "••••••••••••"
                    }
                    className="mt-1.5"
                  />
                  {setupRequired && (
                    <FieldHint>
                      Tip: You can also pre-seed <code className="font-mono">ADMIN_PASSWORD</code> via environment variable on container boot.
                    </FieldHint>
                  )}
                </div>

                {setupRequired && (
                  <div>
                    <Label htmlFor="fc-confirm-password">
                      Confirm Admin Password
                    </Label>
                    <Input
                      id="fc-confirm-password"
                      type="password"
                      autoComplete="new-password"
                      value={confirmPassword}
                      onChange={(e) => {
                        setConfirmPassword(e.target.value);
                        setValidationError(null);
                      }}
                      placeholder="Re-enter password…"
                      className="mt-1.5"
                    />
                  </div>
                )}

                <FieldError message={validationError} />

                <Button
                  type="submit"
                  className="w-full"
                  loading={authMutation.isPending}
                >
                  <KeyRound className="h-4 w-4" />
                  {setupRequired ? "Initialize & Sign In" : "Unlock Console"}
                </Button>
              </form>
            </CardContent>
          </Card>
        </div>
      </div>

      <ComplianceFooter
        version={version}
        repoUrl={repoUrl}
        license={license}
      />
    </div>
  );
}

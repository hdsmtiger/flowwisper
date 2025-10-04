import type { AppProps } from "next/app";
import "../styles.css";

export default function FlowwisperAdminApp({ Component, pageProps }: AppProps) {
  return <Component {...pageProps} />;
}

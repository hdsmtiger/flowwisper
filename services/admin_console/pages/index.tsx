import Head from "next/head";
import styles from "../styles.module.css";

const nextSteps = [
  "接入租户策略、密钥管理模块",
  "配置审计日志查询与导出",
  "集成通知系统与版本分发",
];

export default function Home() {
  return (
    <>
      <Head>
        <title>Flowwisper Admin Console</title>
      </Head>
      <main className={styles.main}>
        <section className={styles.hero}>
          <h1>Flowwisper Admin Console</h1>
          <p>面向企业租户的管理后台脚手架。</p>
        </section>

        <section className={styles.panel}>
          <h2>开发路线</h2>
          <ol>
            {nextSteps.map((step) => (
              <li key={step}>{step}</li>
            ))}
          </ol>
        </section>
      </main>
    </>
  );
}

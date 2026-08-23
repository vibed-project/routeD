import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'routeD',
  tagline: 'A Kubernetes-native semantic router for AI models',
  favicon: 'img/favicon.png',

  // GitHub Pages project site. This works with no DNS setup.
  //
  // To move to a custom domain later: set url to the domain, baseUrl to '/',
  // add a CNAME file to website/static/, and point the DNS at GitHub Pages.
  // Nothing else changes.
  url: 'https://vibed-project.github.io',
  baseUrl: '/routeD/',

  organizationName: 'vibed-project',
  projectName: 'routeD',

  // A broken internal link should fail the build. This site publishes
  // automatically from main, so a warning would go unread.
  onBrokenLinks: 'throw',
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          // The site is built directly from docs/ rather than a copy under
          // website/. routeD already kept its documentation there, and one
          // source of truth means a page cannot drift from the repo.
          path: '../docs',
          sidebarPath: './sidebars.ts',
          routeBasePath: '/',
          editUrl: 'https://github.com/vibed-project/routeD/tree/main/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/social-card.png',
    colorMode: {
      defaultMode: 'light',
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'routeD',
      logo: {
        alt: 'routeD logo',
        src: 'img/routed-logo.png',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'main',
          position: 'left',
          label: 'Docs',
        },
        {
          href: 'https://github.com/vibed-project/routeD',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            {label: 'Overview', to: '/'},
            {label: 'Quickstart', to: '/quickstart'},
            {label: 'How routing works', to: '/how-routing-works'},
            {label: 'Architecture', to: '/architecture'},
          ],
        },
        {
          title: 'Reference',
          items: [
            {label: 'CRDs', to: '/crds'},
            {label: 'Decision API', to: '/decision-api'},
            {label: 'Threat model', to: '/threat-model'},
            {label: 'Decision records', to: '/adr/'},
          ],
        },
        {
          title: 'The stack',
          items: [
            {label: 'hiveD (control plane)', href: 'https://vibed-project.github.io/hiveD/'},
            {label: 'mindD (memory)', href: 'https://vibed-project.github.io/mindD/'},
            {label: 'vibeD (sandbox)', href: 'https://vibed.run/'},
            {label: 'GitHub', href: 'https://github.com/vibed-project/routeD'},
          ],
        },
      ],
      copyright: 'Apache 2.0 · routeD contributors',
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['bash', 'yaml', 'rust', 'json', 'toml'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;

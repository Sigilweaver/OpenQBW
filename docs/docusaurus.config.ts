import { themes as prismThemes } from 'prism-react-renderer';
import type { Config } from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
    title: 'OpenQBW',
    tagline: 'Pure-Rust reader and open specification for Intuit QuickBooks Desktop .QBW files',
    favicon: 'img/favicon.ico',

    markdown: {
        mermaid: true,
        hooks: {
            onBrokenMarkdownLinks: 'warn',
        },
    },
    themes: ['@docusaurus/theme-mermaid'],

    url: 'https://sigilweaver.app',
    baseUrl: '/openqbw/docs/',

    organizationName: 'Sigilweaver',
    projectName: 'OpenQBW',

    onBrokenLinks: 'throw',

    i18n: {
        defaultLocale: 'en',
        locales: ['en'],
    },

    presets: [
        [
            'classic',
            {
                docs: {
                    routeBasePath: '/',
                    sidebarPath: './sidebars.ts',
                    editUrl: 'https://github.com/Sigilweaver/OpenQBW/tree/main/docs/',
                },
                blog: false,
                sitemap: {
                    changefreq: 'weekly',
                    priority: 0.5,
                    filename: 'sitemap.xml',
                },
                theme: {
                    customCss: './src/css/custom.css',
                },
            } satisfies Preset.Options,
        ],
    ],

    themeConfig: {
        metadata: [
            { name: 'keywords', content: 'OpenQBW, QuickBooks, QBW, Intuit, accounting, parser, Rust, Python, migration, SQLite, IIF' },
            { name: 'description', content: 'OpenQBW is a pure-Rust reader for Intuit QuickBooks Desktop .QBW company files. Migrate to CSV, SQLite, or IIF without QuickBooks installed.' },
        ],
        colorMode: {
            defaultMode: 'dark',
            disableSwitch: false,
            respectPrefersColorScheme: true,
        },
        navbar: {
            title: 'Sigilweaver',
            logo: {
                alt: 'Sigilweaver logo',
                src: 'img/logo.svg',
                href: 'https://sigilweaver.app',
                target: '_self',
            },
            items: [
                {
                    type: 'dropdown',
                    label: 'Projects',
                    position: 'left',
                    items: [
                        { label: 'OpenQBW', href: 'https://sigilweaver.app/openqbw/docs/' },
                        { label: 'OpenSQLAnywhere', href: 'https://sigilweaver.app/opensqlanywhere/docs/' },
                        { label: 'OpenProteo', href: 'https://sigilweaver.app/openproteo/docs/' },
                        { label: 'All projects', href: 'https://sigilweaver.app/docs/' },
                    ],
                },
                {
                    href: 'https://github.com/Sigilweaver/OpenQBW',
                    label: 'GitHub',
                    position: 'right',
                },
            ],
        },
        footer: {
            style: 'dark',
            links: [
                {
                    title: 'Project',
                    items: [
                        { label: 'GitHub', href: 'https://github.com/Sigilweaver/OpenQBW' },
                        { label: 'Issues', href: 'https://github.com/Sigilweaver/OpenQBW/issues' },
                        { label: 'crates.io', href: 'https://crates.io/crates/openqbw' },
                        { label: 'PyPI', href: 'https://pypi.org/project/openqbw/' },
                    ],
                },
                {
                    title: 'Related',
                    items: [
                        { label: 'OpenSQLAnywhere', href: 'https://sigilweaver.app/opensqlanywhere/docs/' },
                        { label: 'All projects', href: 'https://sigilweaver.app/docs/' },
                    ],
                },
                {
                    title: 'Legal',
                    items: [
                        { label: 'Terms of Use', href: 'https://sigilweaver.app/terms' },
                        { label: 'Privacy Policy', href: 'https://sigilweaver.app/privacy' },
                    ],
                },
            ],
            copyright: `Copyright ${new Date().getFullYear()} Sigilweaver Holdings LLC. OpenQBW is Apache-2.0 licensed. Documentation licensed under <a href="https://creativecommons.org/licenses/by-sa/4.0/" target="_blank" rel="noopener noreferrer">CC-BY-SA 4.0</a>. QuickBooks is a registered trademark of Intuit Inc.; this project is not affiliated with or endorsed by Intuit.`,
        },
        prism: {
            theme: prismThemes.github,
            darkTheme: prismThemes.dracula,
            additionalLanguages: ['rust', 'toml', 'bash', 'python'],
        },
    } satisfies Preset.ThemeConfig,
};

export default config;

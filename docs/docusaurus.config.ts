import { createConfig } from '@sigilweaver/docs-kit';

export default createConfig({
    title: 'OpenQBW',
    tagline: 'Pure-Rust reader and open specification for Intuit QuickBooks Desktop .QBW files',
    slug: 'openqbw',
    repo: 'OpenQBW',
    keywords: ['OpenQBW', 'QuickBooks', 'QBW', 'Intuit', 'accounting', 'parser', 'Rust', 'Python', 'migration', 'SQLite', 'IIF'],
    description: 'OpenQBW is a pure-Rust reader for Intuit QuickBooks Desktop .QBW company files. Migrate to CSV, SQLite, or IIF without QuickBooks installed.',
    notice: 'QuickBooks is a registered trademark of Intuit Inc.; this project is not affiliated with or endorsed by Intuit.',
    packages: { crate: 'openqbw', pypi: 'openqbw' },
    navbar: {
        projects: { label: 'Projects', items: ['openqbw', 'opensqlanywhere', 'openmassspec', 'all'] },
    },
    footer: {
        related: ['opensqlanywhere', 'all'],
    },
    prismLanguages: ['python'],
});

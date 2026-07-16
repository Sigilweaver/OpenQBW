import type { SidebarsConfig } from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
    docsSidebar: [
        'intro',
        {
            type: 'category',
            label: 'Getting started',
            collapsed: false,
            items: [
                'install',
                'quickstart-cli',
                'quickstart-rust',
                'quickstart-python',
            ],
        },
        {
            type: 'category',
            label: 'Guides',
            items: [
                'use-cases',
                'migration-guide',
                'cli',
                'python-api',
            ],
        },
        {
            type: 'category',
            label: 'Reference',
            items: [
                'format-overview',
                'specification',
            ],
        },
    ],
};

export default sidebars;
